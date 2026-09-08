//! SM status digest (CXA-F341): on a configurable cadence (default 900s, `0`
//! disables) the Scrum Master posts ONE deterministic status report into the
//! team's agents channel — tickets by status with deltas since the previous
//! digest, open PRs, sprint progress, tickets stuck in the same active
//! status past a staleness threshold, and new engine WARN/ERROR events.
//!
//! Hexagonal discipline, per the operator's cost constraint: the only adapter
//! here is the state store (one `load()` snapshot in, one `mutate_state`
//! claim out) and the digest is a PURE function of that snapshot — no
//! engine/LLM call ever happens on the tick, because templated content never
//! justifies token burn. Split from `ceremonies.rs` so a ticket about the
//! digest stops colliding with the ceremony file, exactly like `sm_watch`.

use super::RunCycleUseCase;
use crate::metrics::status_key;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use crate::state::{ProjectState, StatusDigestSnapshot};
use coxagent_domain::Status;
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hash::Hasher;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Consecutive suppressed buckets after which the SM posts one line
/// acknowledging the quiet period — with the 900s default, once an hour.
/// Silence must read as deliberate, never as a dead SM.
const QUIET_ACK_AFTER: u32 = 4;

/// How many open PRs the digest lists by id before it collapses the rest
/// into a "+N more" — a glance, not a wall.
const MAX_PRS_LISTED: usize = 6;

/// How many stuck tickets the digest names before it stops — the worst
/// offenders carry the signal.
const MAX_STUCK_LISTED: usize = 5;

/// Lifecycle order for the counts line — board order, not alphabetical.
const STATUS_ORDER: [Status; 10] = [
    Status::Pending,
    Status::Ready,
    Status::InProgress,
    Status::OnHold,
    Status::Open,
    Status::Fixed,
    Status::Done,
    Status::Verified,
    Status::Documented,
    Status::Rejected,
];

/// The wall-clock bucket id for `now` at `interval_secs` granularity — unix
/// seconds divided by the interval, so a digest is consumed at most once per
/// bucket no matter how many runners or cycles tick inside it. Pure over the
/// timestamp string so tests pin bucket boundaries without a clock.
fn status_bucket(now_rfc3339: &str, interval_secs: u64) -> Option<String> {
    let secs = OffsetDateTime::parse(now_rfc3339, &Rfc3339)
        .ok()?
        .unix_timestamp();
    let interval = i64::try_from(interval_secs).ok()?;
    (interval > 0).then(|| (secs / interval).to_string())
}

/// What one tick of the digest should do. Decided PURELY from the persisted
/// dedup fields and this tick's snapshot hash — and re-decided inside the
/// `mutate_state` write over the fresher fields, so two runners or a restart
/// can never double-post one bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tick {
    /// This bucket was already consumed (by us or another runner).
    SameBucket,
    /// New bucket, board unchanged: stay silent this bucket (`ack` fires the
    /// one-line quiet acknowledgement instead).
    Quiet { ack: bool },
    /// New bucket, board moved: post the full digest.
    Post,
}

/// The pure gate: post only when the bucket advanced AND the snapshot hash
/// changed; count quiet buckets and acknowledge one line after
/// [`QUIET_ACK_AFTER`] of them. Returns the decision and the quiet count to
/// persist (0 after a post or an acknowledgement).
fn gate(last_bucket: &str, bucket: &str, last_hash: u64, hash: u64, quiet: u32) -> (Tick, u32) {
    if last_bucket == bucket {
        return (Tick::SameBucket, quiet);
    }
    if hash == last_hash {
        let quiet = quiet + 1;
        if quiet >= QUIET_ACK_AFTER {
            (Tick::Quiet { ack: true }, 0)
        } else {
            (Tick::Quiet { ack: false }, quiet)
        }
    } else {
        (Tick::Post, 0)
    }
}

/// Statuses the staleness callout may name: not terminal, and not OnHold —
/// a held ticket is parked deliberately and already carries its reason in
/// `hold_reasons`, so calling it stuck would be noise, not signal.
fn is_stuck_candidate(status: Status) -> bool {
    !matches!(
        status,
        Status::Done | Status::Verified | Status::Documented | Status::Rejected | Status::OnHold
    )
}

/// Cumulative engine WARN/ERROR counter across roles (errors + timeouts).
/// The per-role counters only grow, so `now - previous` is exactly "new
/// alerts since the last digest" — durable across restarts for free.
fn alert_total(state: &ProjectState) -> u64 {
    state
        .role_health
        .values()
        .map(|h| h.errors.saturating_add(h.timeouts))
        .sum()
}

/// Tickets sitting in the same active status for at least `stale_hours`,
/// oldest first, worst [`MAX_STUCK_LISTED`] first. Age = now minus the last
/// observable touch (`claimed_at` for claimed work, else `created_at`);
/// tickets with neither timestamp are age-unknown by contract and are never
/// called out rather than guessed about.
fn stuck_tickets(
    state: &ProjectState,
    now: &OffsetDateTime,
    stale_hours: u64,
) -> Vec<(String, &'static str, i64)> {
    if stale_hours == 0 {
        return Vec::new(); // 0 hides the callout line (config doc)
    }
    let threshold = i64::try_from(stale_hours).unwrap_or(i64::MAX);
    let mut stuck: Vec<(i64, String, &'static str)> = state
        .tickets
        .iter()
        .filter(|t| is_stuck_candidate(t.status()))
        .filter_map(|t| {
            let at = t.claimed_at().or(t.created_at())?;
            let at = OffsetDateTime::parse(at, &Rfc3339).ok()?;
            let hours = (*now - at).whole_hours();
            (hours >= threshold).then_some((hours, t.id().to_string(), status_key(t.status())))
        })
        .collect();
    stuck.sort_unstable_by_key(|s| std::cmp::Reverse(s.0));
    stuck.truncate(MAX_STUCK_LISTED);
    stuck.into_iter().map(|(h, id, st)| (id, st, h)).collect()
}

/// Tickets per status key — the ONE counting site: the rendered counts line
/// and the persisted hash/baseline both read it, so they can never drift.
fn status_counts(state: &ProjectState) -> BTreeMap<&'static str, u32> {
    let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
    for t in &state.tickets {
        *counts.entry(status_key(t.status())).or_default() += 1;
    }
    counts
}

/// Freeze the delta baseline (per-status counts + cumulative alerts) and
/// hash the digest-relevant slice of the snapshot into one change detector.
/// Pure. The hash covers exactly what the digest renders — statuses, PRs,
/// sprint, the stuck SET, alert totals — so an unmoved board hashes equal
/// and stays silent, while a board whose rendered lines would differ (even
/// only because a ticket just crossed the staleness threshold) hashes
/// differently and posts. Rendered DELTAS are excluded on purpose: a board
/// that moved and moved back must re-post, and one that merely aged must not.
/// (`std`'s DefaultHasher is not pinned across toolchain releases — an
/// upgrade may re-post one digest; self-healing, harmless.)
fn digest_snapshot(
    state: &ProjectState,
    now: &OffsetDateTime,
    stale_hours: u64,
) -> (u64, StatusDigestSnapshot) {
    let counts = status_counts(state);
    let alerts = alert_total(state);
    let mut canon = String::new();
    let _ = write!(canon, "counts:");
    for (k, v) in &counts {
        let _ = write!(canon, "{k}={v}|");
    }
    let _ = write!(canon, ";alerts={alerts};prs=");
    for pr in &state.open_prs {
        let _ = write!(canon, "#{} {}|", pr.number, pr.title);
    }
    if let Some(sp) = &state.sprint {
        let _ = write!(
            canon,
            ";sprint={}|{}|{}|{}",
            sp.number,
            crate::sprint::done_count(state),
            sp.committed.len(),
            sp.goal
        );
    }
    let _ = write!(canon, ";stuck=");
    for (id, _, _) in stuck_tickets(state, now, stale_hours) {
        let _ = write!(canon, "{id}|");
    }
    let mut hasher = DefaultHasher::new();
    hasher.write(canon.as_bytes());
    let baseline = StatusDigestSnapshot {
        counts: counts
            .iter()
            .map(|(k, v)| ((*k).to_owned(), *v))
            .collect(),
        alerts,
    };
    (hasher.finish(), baseline)
}

/// `3d2h`-style compact age for the stuck callout.
fn age_label(hours: i64) -> String {
    if hours >= 24 {
        format!("{}d{}h", hours / 24, hours % 24)
    } else {
        format!("{hours}h")
    }
}

/// Build the digest message. Pure over the snapshot, the previous baseline
/// and the timestamps — the same state always renders the same digest.
fn status_digest(
    state: &ProjectState,
    prev: &StatusDigestSnapshot,
    now: &str,
    now_dt: &OffsetDateTime,
    stale_hours: u64,
) -> String {
    let first_ever = prev.is_empty();
    let mut out = format!(
        "**SM status digest** · {}\n",
        &now[..16.min(now.len())]
    );

    // Tickets by status with deltas since the previous digest.
    let counts = status_counts(state);
    let parts: Vec<String> = STATUS_ORDER
        .iter()
        .filter_map(|s| {
            let n = counts.get(status_key(*s)).copied()?;
            let mut part = format!("{} {n}", status_key(*s));
            if !first_ever {
                let was = prev.counts.get(status_key(*s)).copied().unwrap_or(0);
                match n.cmp(&was) {
                    Ordering::Greater => {
                        let _ = write!(part, " (+{})", n - was);
                    }
                    Ordering::Less => {
                        let _ = write!(part, " (-{})", was - n);
                    }
                    Ordering::Equal => {}
                }
            }
            Some(part)
        })
        .collect();
    if parts.is_empty() {
        out.push_str("- Tickets: none yet — BA proposals fill the board\n");
    } else {
        let _ = writeln!(out, "- Tickets: {}", parts.join(" · "));
    }

    // Open PRs: count first, ids/titles behind it.
    if state.open_prs.is_empty() {
        out.push_str("- PRs open: 0 — nothing awaiting review\n");
    } else {
        let total = state.open_prs.len();
        let listed: Vec<String> = state
            .open_prs
            .iter()
            .take(MAX_PRS_LISTED)
            .map(|p| format!("#{} {}", p.number, p.title))
            .collect();
        let mut line = format!("- PRs open: {total} — {}", listed.join(", "));
        if total > MAX_PRS_LISTED {
            let _ = write!(line, " … (+{} more)", total - MAX_PRS_LISTED);
        }
        let _ = writeln!(out, "{line}");
    }

    // Sprint: committed vs done, exactly the daily digest's definition.
    if let Some(sp) = &state.sprint {
        let _ = writeln!(
            out,
            "- Sprint {}: {}/{} committed done — goal: {}",
            sp.number,
            crate::sprint::done_count(state),
            sp.committed.len(),
            sp.goal
        );
    } else {
        out.push_str("- Sprint: none active\n");
    }

    // Stuck: named by id, worst first — the reason the SM exists.
    if stale_hours > 0 {
        let stuck = stuck_tickets(state, now_dt, stale_hours);
        if !stuck.is_empty() {
            let items: Vec<String> = stuck
                .iter()
                .map(|(id, st, h)| format!("{id} ({st}, {})", age_label(*h)))
                .collect();
            let _ = writeln!(out, "- Stuck >{stale_hours}h: {}", items.join(", "));
        }
    }

    // New engine WARN/ERROR events since the previous digest.
    if !first_ever {
        let fresh = alert_total(state).saturating_sub(prev.alerts);
        if fresh > 0 {
            let _ = writeln!(out, "- Engine alerts since last digest: +{fresh} (errors/timeouts)");
        }
    }
    out
}

/// The one-line quiet acknowledgement — a long silent stretch must say WHY
/// it is silent, in a single line.
fn quiet_ack(suppressed: u32) -> String {
    format!("**SM status digest** — quiet: the board hasn't moved for the last {suppressed} checks.")
}

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// The SM's periodic status report (CXA-F341). One digest per configured
    /// interval, posted only when the board moved since the previous one; a
    /// long quiet period gets a one-line acknowledgement instead of silence.
    /// Deterministic state snapshot in, chat post out — no engine call.
    pub(super) async fn sm_status_report(&self) {
        let cadence = &self.config.workflow.cadence;
        let interval = cadence.status_report_interval_secs;
        if interval == 0 {
            return; // 0 disables the digest entirely (CXA-F341)
        }
        let stale_hours = cadence.status_stale_hours;
        let now = crate::state::now_rfc3339();
        let (Some(bucket), Some(now_dt)) = (
            status_bucket(&now, interval),
            OffsetDateTime::parse(&now, &Rfc3339).ok(),
        ) else {
            return; // unreadable clock string — skip the tick, never spam
        };
        let Ok(state) = self.store.load().await else {
            return;
        };
        let (hash, snapshot) = digest_snapshot(&state, &now_dt, stale_hours);
        let (tick, _) = gate(
            &state.last_status_bucket,
            &bucket,
            state.last_status_hash,
            hash,
            state.status_digest_quiet,
        );
        let msg = match tick {
            Tick::SameBucket => return,
            // Quiet buckets post nothing here; the acknowledgement line (when
            // it fires) is built inside the mutate from the FRESH counter, so
            // a concurrent quiet tick can never make it undercount.
            Tick::Quiet { .. } => String::new(),
            Tick::Post => status_digest(&state, &state.last_status_snapshot, &now, &now_dt, stale_hours),
        };
        drop(state);
        // The in-mutate re-gate over the fresher fields is the claim: only
        // the writer that still sees an unconsumed bucket posts it.
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            let (fresh, quiet_now) = gate(
                &s.last_status_bucket,
                &bucket,
                s.last_status_hash,
                hash,
                s.status_digest_quiet,
            );
            if fresh != tick {
                return Ok(()); // another runner consumed the bucket — next tick decides
            }
            s.last_status_bucket.clone_from(&bucket);
            match tick {
                // Unreachable (SameBucket returned before the mutate); kept
                // for exhaustiveness.
                Tick::SameBucket => {}
                Tick::Quiet { ack } => {
                    if ack {
                        // Read the counter BEFORE the reset below: the line
                        // reports the suppression count this write consumes.
                        let line = quiet_ack(s.status_digest_quiet + 1);
                        s.post_chat_in(
                            "SM",
                            &format!("📊 {line}"),
                            crate::state::AGENTS_CHANNEL,
                            Vec::new(),
                        );
                        s.log_activity("SM", "acknowledged a quiet board", None);
                    }
                    s.status_digest_quiet = quiet_now;
                }
                Tick::Post => {
                    s.last_status_hash = hash;
                    s.last_status_snapshot.clone_from(&snapshot);
                    s.status_digest_quiet = 0;
                    s.post_chat_in(
                        "SM",
                        &format!("📊 {msg}"),
                        crate::state::AGENTS_CHANNEL,
                        Vec::new(),
                    );
                    s.log_activity("SM", "posted the status digest", None);
                }
            }
            Ok(())
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::RoleHealth;
    use coxagent_domain::{Ticket, TicketId, TicketType};
    use std::path::PathBuf;
    use std::sync::Arc;

    const NOW: &str = "2026-09-09T07:15:00Z";

    fn ticket(id: &str, ty: TicketType, status: Status) -> Ticket {
        let json = serde_json::json!({
            "id": id, "type": match ty {
                TicketType::Feature => "feature",
                TicketType::Bug => "bug",
                TicketType::Chore => "chore",
            },
            "title": "t", "description": "",
            "priority": "medium", "complexity": "small", "status": status_key(status),
            "has_ui": false, "design": {"technical": null, "ux": null},
            "parent_id": null, "depends_on": []
        });
        serde_json::from_value(json).expect("ticket")
    }

    fn ticket_created_at(id: &str, status: Status, at: &str) -> Ticket {
        let json = serde_json::json!({
            "id": id, "type": "bug", "title": "t", "description": "",
            "priority": "medium", "complexity": "small", "status": status_key(status),
            "has_ui": false, "design": {"technical": null, "ux": null},
            "parent_id": null, "depends_on": [], "created_at": at
        });
        serde_json::from_value(json).expect("ticket")
    }

    fn pr(number: u64, title: &str) -> crate::ports::outbound::PrOpen {
        crate::ports::outbound::PrOpen {
            number,
            title: title.to_owned(),
            head: "head".to_owned(),
            base: "main".to_owned(),
            url: String::new(),
            author: "dev".to_owned(),
            ci: "passing".to_owned(),
            mergeable: true,
            created: NOW.to_owned(),
        }
    }

    fn now_dt() -> OffsetDateTime {
        OffsetDateTime::parse(NOW, &Rfc3339).expect("now")
    }

    fn fixture_state() -> ProjectState {
        let mut state = ProjectState::default();
        state.tickets = vec![
            ticket("F001", TicketType::Feature, Status::Ready),
            ticket("F002", TicketType::Feature, Status::InProgress),
            ticket("F003", TicketType::Feature, Status::InProgress),
            ticket_created_at("B001", Status::Open, "2026-09-05T03:15:00Z"),
        ];
        state.open_prs = vec![pr(587, "Fix auth refresh"), pr(639, "Add shard store")];
        state.sprint = Some(crate::state::Sprint {
            number: 12,
            goal: "Ship REST runner".to_owned(),
            started_cycle: 1,
            length_cycles: 10,
            committed: vec![
                TicketId::new("F001").expect("id"),
                TicketId::new("F002").expect("id"),
                TicketId::new("F003").expect("id"),
            ],
            started_at: NOW.to_owned(),
            bug_burn_floor: None,
        });
        state.role_health.insert(
            "SA".to_owned(),
            RoleHealth {
                errors: 4,
                timeouts: 1,
                ..RoleHealth::default()
            },
        );
        state
    }

    #[test]
    fn the_gate_posts_only_when_the_bucket_and_the_board_move() {
        // First tick: nothing persisted yet — the baseline digest posts.
        assert_eq!(gate("", "b1", 0, 7, 0), (Tick::Post, 0));
        // Same bucket: already consumed, never twice.
        assert_eq!(gate("b1", "b1", 7, 7, 0), (Tick::SameBucket, 0));
        // Bucket advanced, board unchanged: quiet, counting up to the ack.
        assert_eq!(gate("b1", "b2", 7, 7, 0), (Tick::Quiet { ack: false }, 1));
        assert_eq!(gate("b2", "b3", 7, 7, 1), (Tick::Quiet { ack: false }, 2));
        assert_eq!(gate("b3", "b4", 7, 7, 2), (Tick::Quiet { ack: false }, 3));
        // Fourth suppressed bucket: one line acknowledging the quiet.
        assert_eq!(gate("b4", "b5", 7, 7, 3), (Tick::Quiet { ack: true }, 0));
        // The acknowledgement restarted the count.
        assert_eq!(gate("b5", "b6", 7, 7, 0), (Tick::Quiet { ack: false }, 1));
        // Board moved: post, and the quiet count resets.
        assert_eq!(gate("b6", "b7", 7, 8, 1), (Tick::Post, 0));
    }

    #[test]
    fn the_digest_reports_counts_deltas_prs_sprint_stuck_and_alerts() {
        let state = fixture_state();
        let prev = StatusDigestSnapshot {
            counts: BTreeMap::from([
                ("ready".to_owned(), 1),
                ("in_progress".to_owned(), 1),
                ("open".to_owned(), 1),
            ]),
            alerts: 3,
        };
        let md = status_digest(&state, &prev, NOW, &now_dt(), 48);
        assert!(md.starts_with("**SM status digest** · 2026-09-09T07:15"));
        assert!(md.contains("- Tickets: ready 1 · in_progress 2 (+1) · open 1"));
        assert!(md.contains("- PRs open: 2 — #587 Fix auth refresh, #639 Add shard store"));
        assert!(md.contains("- Sprint 12: 0/3 committed done — goal: Ship REST runner"));
        // B001 sat in Open for exactly 100h: past the 48h threshold.
        assert!(md.contains("- Stuck >48h: B001 (open, 4d4h)"));
        assert!(md.contains("- Engine alerts since last digest: +2 (errors/timeouts)"));
    }

    #[test]
    fn the_first_digest_is_a_baseline_without_deltas() {
        let state = fixture_state();
        let md = status_digest(&state, &StatusDigestSnapshot::default(), NOW, &now_dt(), 48);
        assert!(md.contains("- Tickets: ready 1 · in_progress 2 · open 1"));
        assert!(!md.contains("(+"));
        assert!(!md.contains("(-"));
        assert!(!md.contains("Engine alerts"));
    }

    #[test]
    fn a_zero_staleness_threshold_hides_the_stuck_callout() {
        let state = fixture_state();
        let md = status_digest(&state, &StatusDigestSnapshot::default(), NOW, &now_dt(), 0);
        assert!(!md.contains("Stuck"));
        // …and the threshold is part of the change detector: with 0 the
        // stale ticket never enters the hash.
        let (h0, _) = digest_snapshot(&state, &now_dt(), 0);
        let (h48, _) = digest_snapshot(&state, &now_dt(), 48);
        assert_ne!(h0, h48);
    }

    #[test]
    fn the_hash_tracks_every_change_the_digest_would_render() {
        let state = fixture_state();
        let (h1, snap1) = digest_snapshot(&state, &now_dt(), 48);
        let (h2, snap2) = digest_snapshot(&state, &now_dt(), 48);
        assert_eq!(h1, h2);
        assert_eq!(snap1, snap2);
        // A status transition moves the hash…
        let mut moved = fixture_state();
        moved.tickets[0] = ticket("F001", TicketType::Feature, Status::InProgress);
        assert_ne!(digest_snapshot(&moved, &now_dt(), 48).0, h1);
        // …so does a new PR, a sprint change, a fresh alert, and a ticket
        // crossing the staleness threshold (47h → not stuck, 48h → stuck).
        let mut prd = fixture_state();
        prd.open_prs.push(pr(700, "One more"));
        assert_ne!(digest_snapshot(&prd, &now_dt(), 48).0, h1);
        let mut young = fixture_state();
        young.tickets[3] = ticket_created_at("B001", Status::Open, "2026-09-07T08:15:00Z");
        let (young_hash, _) = digest_snapshot(&young, &now_dt(), 48);
        let mut old = young.clone();
        old.tickets[3] = ticket_created_at("B001", Status::Open, "2026-09-07T07:15:00Z");
        assert_ne!(digest_snapshot(&old, &now_dt(), 48).0, young_hash);
    }

    #[test]
    fn the_quiet_acknowledgement_is_one_line_that_says_why() {
        let ack = quiet_ack(4);
        assert_eq!(ack.lines().count(), 1);
        assert!(ack.contains("quiet"));
        assert!(ack.contains('4'));
    }

    #[tokio::test]
    async fn the_first_tick_posts_and_stamps_and_the_same_bucket_stays_silent() {
        let store = Arc::new(super::super::cycle_tests::MemStore::default());
        // One ticket on the board so the persisted snapshot carries data.
        store
            .state
            .lock()
            .expect("lock")
            .tickets
            .push(ticket("F001", TicketType::Feature, Status::Ready));
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(super::super::cycle_tests::RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        );
        uc.sm_status_report().await;
        let s1 = store.load().await.expect("load");
        assert_eq!(s1.chat.len(), 1);
        assert_eq!(s1.chat[0].user, "SM");
        assert_eq!(s1.chat[0].channel, crate::state::AGENTS_CHANNEL);
        assert!(s1.chat[0].body.contains("**SM status digest**"));
        assert!(s1.chat[0].body.contains("ready 1"));
        assert!(!s1.last_status_bucket.is_empty());
        assert_ne!(s1.last_status_hash, 0);
        assert!(!s1.last_status_snapshot.is_empty());
        // Same bucket again: the gate consumes nothing, nothing is posted.
        uc.sm_status_report().await;
        let s2 = store.load().await.expect("load");
        assert_eq!(s2.chat.len(), 1);
        assert_eq!(s2.last_status_bucket, s1.last_status_bucket);
    }

    #[tokio::test]
    async fn a_new_bucket_over_an_unchanged_board_stays_silent_but_consumes_the_bucket() {
        let store = Arc::new(super::super::cycle_tests::MemStore::default());
        store
            .state
            .lock()
            .expect("lock")
            .tickets
            .push(ticket("F001", TicketType::Feature, Status::Ready));
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(super::super::cycle_tests::RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        );
        uc.sm_status_report().await; // baseline post; real bucket stamped
        // Wind the persisted bucket into the past WITHOUT touching the hash:
        // the next tick lands in a fresh bucket over an unchanged board —
        // the quiet path, deterministically.
        store.state.lock().expect("lock").last_status_bucket = "0".to_owned();
        uc.sm_status_report().await;
        let s = store.load().await.expect("load");
        assert_eq!(s.chat.len(), 1, "an unmoved board must not post");
        assert_ne!(s.last_status_bucket, "0", "the quiet bucket IS consumed");
        assert_eq!(s.status_digest_quiet, 1, "the suppression is counted");
    }

    #[tokio::test]
    async fn the_quiet_acknowledgement_fires_after_enough_suppressed_buckets() {
        let store = Arc::new(super::super::cycle_tests::MemStore::default());
        store
            .state
            .lock()
            .expect("lock")
            .tickets
            .push(ticket("F001", TicketType::Feature, Status::Ready));
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(super::super::cycle_tests::RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        );
        uc.sm_status_report().await; // baseline post; real bucket stamped
        {
            let mut st = store.state.lock().expect("lock");
            st.last_status_bucket = "0".to_owned();
            st.status_digest_quiet = QUIET_ACK_AFTER - 1;
        }
        uc.sm_status_report().await;
        let s = store.load().await.expect("load");
        assert_eq!(s.chat.len(), 2, "the ack is the one quiet-period line");
        assert_eq!(s.chat[1].user, "SM");
        assert!(s.chat[1].body.contains("quiet"));
        assert_eq!(
            s.status_digest_quiet, 0,
            "the ack restarts the suppression count"
        );
    }
}
