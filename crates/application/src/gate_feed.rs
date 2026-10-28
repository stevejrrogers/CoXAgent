//! Gate-feed integrity (CXA-F382): a review gate whose forge evidence feed
//! cannot answer records **blocked**, never a letter grade computed from
//! missing evidence. This module is the pure decision core: classify the
//! port failure, decide block-vs-grade-with-cached-evidence, track
//! consecutive blocks (two → suspend grading until the feed answers), stamp
//! evidence age, and let metrics exclude blocked evaluations so no
//! zero-denominator artifacts appear. All IO happens at the caller
//! (`ForgePort` adapters / the review lane); every function here is pure.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long cached evidence may still drive a grade before the gate must
/// block instead. Overridable per project (AC4 calls it "configurable").
pub const DEFAULT_FRESHNESS_WINDOW_HOURS: u64 = 24;

/// After this many consecutive blocked evaluations of the same gate, grading
/// is suspended and the next cycle's plan must name restoring the feed first.
pub const BLOCKS_BEFORE_SUSPENSION: u32 = 2;

/// Why the forge feed could not answer — AC1 requires the error class on the
/// verdict. Derived purely from the port error string (adapters embed the
/// HTTP/CLI detail there), keying on domain-visible signals, never literals
/// alone: anything unrecognized is conservatively `Unreachable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedFault {
    /// The provider answered with a rate limit (HTTP 403/429, `rate limit`).
    RateLimited,
    /// DNS/socket/timeout/auth-dead — the feed simply did not answer.
    Unreachable,
}

impl FeedFault {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            FeedFault::RateLimited => "rate-limited",
            FeedFault::Unreachable => "unreachable",
        }
    }

    /// Classify a [`crate::PortError`] message. Private-classification seam:
    /// kept over the rendered message rather than a new PortError variant so
    /// every existing adapter gains the taxonomy without a breaking change.
    #[must_use]
    pub fn classify(msg: &str) -> Self {
        let m = msg.to_ascii_lowercase();
        if m.contains("rate limit") || m.contains("rate-limit") || m.contains("429")
            || m.contains("secondary rate") || m.contains("api rate")
        {
            FeedFault::RateLimited
        } else {
            FeedFault::Unreachable
        }
    }
}

/// One recorded gate evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Evidence fresh + feed answered: normal graded run (carries nothing).
    Graded,
    /// Fed from cache inside the freshness window — grades, but stamped.
    GradedStale { age_hours: u64 },
    /// The gate refused to judge.
    Blocked { reason: String },
}

impl Verdict {
    /// The human/cycle-facing text (AC1 + AC4 wording verbatim).
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Verdict::Graded => "graded".to_owned(),
            Verdict::GradedStale { age_hours } => format!("graded — evidence age: {age_hours}h"),
            Verdict::Blocked { reason } => format!("blocked: {reason}"),
        }
    }

    #[must_use]
    pub fn is_blocked(&self) -> bool {
        matches!(self, Verdict::Blocked { .. })
    }
}

/// History entry pairing a verdict with both of its timestamps (AC5 shows the
/// blocked→graded transition with when it blocked and when it recovered).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictRecord {
    pub verdict: Verdict,
    /// RFC-3339 wall-clock stamp supplied by the caller at evaluation time.
    pub at: String,
    /// Seconds since epoch of the same instant — for age arithmetic without
    /// re-parsing the stamp in tests/adapters.
    pub at_epoch: u64,
}

/// What AC1 fixes verbatim; assembled once so every surface renders the same
/// WHY (gate text, waiting-for-me card, cycle plan note).
#[must_use]
pub fn blocked_reason(fault: FeedFault) -> String {
    format!(
        "delivery history unreachable ({}) — unblocked by restoring the forge feed",
        fault.label()
    )
}

/// Decide the verdict for one gate evaluation (AC1 + AC4).
///
/// * `feed_result` — `Err` when the delivery-history read failed this round.
/// * `cached_at_epoch` — when the evidence we would grade on was fetched;
///   `None` when there is no cache at all.
/// * `window_hours` — freshness window (default [`DEFAULT_FRESHNESS_WINDOW_HOURS`]).
#[must_use]
pub fn evaluate(
    feed_result: Result<(), &str>,
    cached_at_epoch: Option<u64>,
    now_epoch: u64,
    window_hours: u64,
) -> Verdict {
    match feed_result {
        Ok(()) => match cached_at_epoch {
            None | Some(_) => {
                // Fresh read wins; the stamp reflects reality either way.
                let age = cached_at_epoch.map_or(0, |t| now_epoch.saturating_sub(t) / 3600);
                if age == 0 {
                    Verdict::Graded
                } else {
                    Verdict::GradedStale { age_hours: age }
                }
            }
        },
        Err(msg) => {
            let fault = FeedFault::classify(msg);
            match cached_at_epoch {
                Some(t) if now_epoch.saturating_sub(t) < window_hours * 3600 => {
                    Verdict::GradedStale {
                        age_hours: now_epoch.saturating_sub(t) / 3600,
                    }
                }
                _ => Verdict::Blocked {
                    reason: blocked_reason(fault),
                },
            }
        }
    }
}

/// Mutable bookkeeping for one gate's feed health. Lives on `ProjectState`;
/// cheap, small, and serde-friendly so history survives restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateFeedMonitor {
    /// Consecutive blocked evaluations of this gate (AC3 counts these).
    pub consecutive_blocks: u32,
    /// Epoch second of the last successful feed read (last-successful-check
    /// attribution on the blocked verdict / waiting-for-me card).
    pub last_success_epoch: Option<u64>,
    /// Set while grading is suspended (AC3/AC5).
    pub suspended_since_epoch: Option<u64>,
    /// Newest last — bounded by [`Self::record`].
    pub history: Vec<VerdictRecord>,
}

const HISTORY_CAP: usize = 50;

impl GateFeedMonitor {
    /// Record one evaluation and return its verdict. Suspension flips on at
    /// the second consecutive block and clears on the first good read.
    pub fn record(&mut self, outcome: Result<(), &str>, cached_at: Option<u64>, now_epoch: u64, window_hours: u64) -> Verdict {
        let v = evaluate(outcome, cached_at, now_epoch, window_hours);
        match &v {
            Verdict::Graded | Verdict::GradedStale { .. } => {
                self.consecutive_blocks = 0;
                self.last_success_epoch = Some(now_epoch);
                self.suspended_since_epoch = None;
            }
            Verdict::Blocked { .. } => {
                self.consecutive_blocks += 1;
                if self.consecutive_blocks >= BLOCKS_BEFORE_SUSPENSION && self.suspended_since_epoch.is_none() {
                    self.suspended_since_epoch = Some(now_epoch);
                }
            }
        }
        self.history.push(VerdictRecord { verdict: v.clone(), at: rfc3339(now_epoch), at_epoch: now_epoch });
        if self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
        v
    }

    /// AC3: `Some(action)` when the next cycle's plan must lead with feed
    /// repair — after two consecutive blocks, or any time suspended.
    #[must_use]
    pub fn plan_first_action(&self) -> Option<&'static str> {
        if self.suspended_since_epoch.is_some() || self.consecutive_blocks >= BLOCKS_BEFORE_SUSPENSION {
            Some("restore the forge evidence feed")
        } else {
            None
        }
    }

    /// Age of the newest usable evidence in hours (for UI attribution).
    #[must_use]
    pub fn evidence_age_hours(&self, now_epoch: u64) -> Option<u64> {
        self.last_success_epoch.map(|t| now_epoch.saturating_sub(t) / 3600)
    }

    /// AC5-exclusion: scores recorded while this gate was blocked (or had
    /// never succeeded) are not useful-run/velocity evidence. A blocked tail
    /// appended to history does NOT make earlier graded scores unusable —
    /// exclusion is keyed to the evaluation moment, hence the parameter.
    #[must_use]
    pub fn excludes_score_at(&self, score_epoch: u64) -> bool {
        match self.suspended_since_epoch {
            Some(since) => score_epoch >= since,
            None => false,
        }
    }
}

/// Minimal RFC-3339 (UTC, second precision) without pulling chrono into the
/// app crate's hot path; matches the `at` strings the rest of state already
/// stores well enough for display ordering within a single hub clock.
#[allow(clippy::many_single_char_names)]
#[must_use]
pub fn rfc3339(epoch: u64) -> String {
    let days = epoch / 86_400;
    let secs_of_day = epoch % 86_400;
    let (h, m, s) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
    // Civil-from-days (Howard Hinnant's algorithm) — pure, no leap table.
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Current epoch seconds — the ONE clock read, at the call boundary.
#[must_use]
pub fn now_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    const NOW: u64 = 1_758_000_000; // fixed instant, deterministic tests

    #[test]
    fn ac1_unreachable_feed_blocks_with_named_reason_and_class() {
        let v = evaluate(Err("gh pr list failed: dial tcp: connection refused"), None, NOW, 24);
        assert_eq!(
            v.render(),
            "blocked: delivery history unreachable (unreachable) — unblocked by restoring the forge feed"
        );
    }

    #[test]
    fn ac1_rate_limited_is_classified_not_generic() {
        assert_eq!(FeedFault::classify("gh api failed: 429 too many requests"), FeedFault::RateLimited);
        assert_eq!(FeedFault::classify("gh api failed: API rate limit exceeded"), FeedFault::RateLimited);
        assert_eq!(FeedFault::classify("dial tcp: lookup github.com: no such host"), FeedFault::Unreachable);
    }

    #[test]
    fn ac1_monitor_tracks_last_success_for_attribution() {
        let mut m = GateFeedMonitor::default();
        let _ = m.record(Ok(()), None, NOW - 7200, 24);
        let v = m.record(Err("boom"), None, NOW, 24);
        assert!(v.is_blocked());
        assert_eq!(m.last_success_epoch, Some(NOW - 7200));
        assert_eq!(m.evidence_age_hours(NOW), Some(2));
    }

    #[test]
    fn ac2_block_report_is_one_shared_string() {
        assert!(blocked_reason(FeedFault::RateLimited).contains("restoring the forge feed"));
    }

    #[test]
    fn ac3_two_consecutive_blocks_suspend_and_name_the_repair_action() {
        let mut m = GateFeedMonitor::default();
        let first = m.record(Err("down"), None, NOW, 24);
        assert!(first.is_blocked());
        assert_eq!(m.plan_first_action(), None, "one block warns, does not suspend");
        let second = m.record(Err("down again"), None, NOW + 60, 24);
        assert!(second.is_blocked());
        assert_eq!(m.plan_first_action(), Some("restore the forge evidence feed"));
        assert_eq!(m.suspended_since_epoch, Some(NOW + 60));
    }

    #[test]
    fn ac4_cached_evidence_inside_window_grades_with_age_stamp() {
        let v = evaluate(Err("rate limited: secondary rate limit hit"), Some(NOW - 5 * 3600), NOW, 24);
        assert_eq!(v, Verdict::GradedStale { age_hours: 5 });
        assert!(v.render().contains("evidence age: 5h"));
        // Outside the window the same cache stops being trustworthy.
        let old = evaluate(Err("rate limited: secondary rate limit hit"), Some(NOW - 30 * 3600), NOW, 24);
        assert!(old.is_blocked());
    }

    #[test]
    fn ac4_freshness_window_is_configurable() {
        let v = evaluate(Err("connection refused"), Some(NOW - 30 * 3600), NOW, 48);
        assert!(!v.is_blocked(), "a wider window keeps 30h-old evidence gradable");
        assert_eq!(v, Verdict::GradedStale { age_hours: 30 });
    }

    #[test]
    fn ac5_recovery_reruns_and_records_both_timestamps() {
        let mut m = GateFeedMonitor::default();
        let _ = m.record(Err("down"), None, NOW, 24);
        let _ = m.record(Err("down"), None, NOW + 60, 24);
        let ok = m.record(Ok(()), None, NOW + 120, 24);
        assert_eq!(ok, Verdict::Graded);
        assert_eq!(m.consecutive_blocks, 0, "a good read auto-clears suspension");
        assert_eq!(m.suspended_since_epoch, None);
        // Transition pair, oldest→newest, both stamped.
        let hist = &m.history;
        assert!(matches!(hist[0].verdict, Verdict::Blocked { .. }));
        assert_eq!(hist[0].at_epoch, NOW);
        assert!(matches!(hist[1].verdict, Verdict::Blocked { .. }), "second block suspends");
        assert_eq!(hist[2].verdict, Verdict::Graded);
        assert_eq!(hist[2].at_epoch, NOW + 120);
    }

    #[test]
    fn ac5_scores_from_a_suspended_span_are_excluded_from_metrics() {
        let mut m = GateFeedMonitor::default();
        let _ = m.record(Ok(()), None, NOW - 10_000, 24); // healthy era
        let _ = m.record(Err("down"), None, NOW, 24);
        let _ = m.record(Err("down"), None, NOW + 60, 24); // suspended since NOW+60
        assert!(!m.excludes_score_at(NOW - 5_000), "healthy-era scores stay useful");
        assert!(m.excludes_score_at(NOW + 70), "scores cut while suspended are excluded");
        assert!(!m.excludes_score_at(NOW - 20_000), "pre-suspension history untouched");
    }

    #[test]
    fn guard_grading_pin_stays_untouched_by_the_feed_path() {
        // The F382 pre-work pin: an evidenced team keeps the exact prior
        // behaviour — verdict Graded, no stale stamp, monitor not consulted.
        let v = evaluate(Ok(()), None, NOW, 24);
        assert_eq!(v, Verdict::Graded);
    }

    #[test]
    fn rfc3339_formats_deterministically() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_758_000_000), "2025-09-16T05:20:00Z");
    }
}
