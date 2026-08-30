//! Human governance-attention analytics (CXA-F230). Pure aggregation over the
//! append-only intervention ledger — the operator-side mirror of the
//! agent-side metrics in [`crate::metrics_health`].
//!
//! Unit of measure, honestly stated: attention is counted in DECISIONS —
//! each recorded gate action is one measured moment a person had to stop and
//! look. Wall-clock seconds per decision are not recorded anywhere and are
//! deliberately not fabricated here.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::state::{InterventionRecord, ProjectState};
use coxagent_domain::{InterventionKind, TicketType};

/// The rolling window the trend series and the anomaly check run over.
pub const ATTENTION_WINDOW_DAYS: usize = 14;
/// The anomaly's baseline: everything in the window except the trailing
/// [`RECENT_DAYS`]. The split is pinned so the two constants stay coherent.
const BASELINE_DAYS: usize = ATTENTION_WINDOW_DAYS - RECENT_DAYS;
const RECENT_DAYS: usize = 3;
/// The `N` in "exceeds N standard deviations above the trailing average".
const ANOMALY_SIGMA: f64 = 2.0;
/// Smallest recent burst considered — below this, single decisions are noise,
/// not an anomaly (a brand-new project must not alert on its first gate).
const MIN_RECENT_INTERVENTIONS: u64 = 3;

/// One area's per-kind attention row, zero-filled so old clients and charts
/// always see a complete `Feature|Bug|Chore × kinds` grid.
pub type AttentionRow = BTreeMap<String, u64>;

/// The attributed view of the operator's own review effort (CXA-F230): counts
/// per ticket class and intervention kind, the ledger total, and the explicit
/// `unattributed` bucket for records that could not be attributed. Attributed
/// volume is derivable as `interventions_total - unattributed`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AttentionSummary {
    /// Ticket-class key (`feature`|`bug`|`chore`) → per-kind counts. All
    /// three areas are always present, zero-filled when untouched.
    pub attention_by_area: BTreeMap<String, AttentionRow>,
    /// Every deduped decision in the ledger, attributed or not.
    pub interventions_total: u64,
    /// Records lacking a derivable ticket class (owner or area unknown) —
    /// reported explicitly, never silently assigned or dropped (AC4).
    pub unattributed: u64,
    /// The AC3 attention-vs-delivered-value anomaly, when one fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anomaly: Option<AttentionAnomaly>,
}

/// One area's attention burst that bought no delivered value: the recent
/// cumulative exceeded `ANOMALY_SIGMA` standard deviations above the area's
/// trailing daily average while the window closed zero verified tickets of
/// that class.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttentionAnomaly {
    /// Ticket-class key the burst concentrated on.
    pub area: String,
    /// Decisions across the trailing [`RECENT_DAYS`].
    pub recent_interventions: u64,
    /// Mean daily decisions across the baseline days.
    pub baseline_mean: f64,
    /// Population stddev across the baseline days.
    pub baseline_stddev: f64,
    /// The threshold that fired (`baseline_mean + N * stddev`).
    pub sigma_threshold: f64,
    /// Verified tickets of this class inside the window — 0 is the "flat
    /// delivered-value" half of the condition.
    pub verified_in_window: u64,
}

/// One day of the attention time-series (the compute_trends point shape).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttentionTrendPoint {
    pub day: String,
    /// Decisions recorded that day (attributed + unattributed).
    pub interventions: f64,
    /// Decisions that day that could not be attributed to a ticket class.
    pub unattributed: f64,
}

/// Time-series response, matching [`crate::metrics_health::CycleTrendsResponse`]'s
/// shape so the dashboard renders it with the same chart plumbing. Empty
/// windows trend as zeroes once anything has been recorded; a project with
/// no recorded gates at all reports `insufficient_data` instead of a chart
/// of nothing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttentionTrendsResponse {
    pub insufficient_data: bool,
    pub points: Vec<AttentionTrendPoint>,
}

/// Calendar day (`YYYY-MM-DD`) from an RFC3339 timestamp; best-effort.
fn day_of(at: &str) -> String {
    at.split('T').next().unwrap_or(at).to_owned()
}

/// A ticket's class from its id alone (the letter code is the last dash
/// segment's head) — so delivered value still attributes after a ticket is
/// pruned from state. Unrecognised ids attribute nothing.
fn ticket_class_of_id(id: &str) -> Option<TicketType> {
    let seg = id.rsplit('-').next().unwrap_or(id);
    match seg.chars().next()? {
        'F' => Some(TicketType::Feature),
        'B' => Some(TicketType::Bug),
        'C' => Some(TicketType::Chore),
        _ => None,
    }
}

/// The ledger collapsed to one record per decision identity: concurrent
/// resolution of the same item double-writes the same decision (a lost-update
/// race between two readers of the same pre-decision state), and aggregation
/// must count that effort once (AC5). Genuinely separate decisions of the
/// same item differ in time by minutes and keep counting.
fn deduped(state: &ProjectState) -> Vec<&InterventionRecord> {
    let mut seen: BTreeSet<(&'static str, &str, &str, &str)> = BTreeSet::new();
    state
        .governance_interventions
        .iter()
        .filter(|r| seen.insert(r.decision_identity()))
        .collect()
}

/// Aggregate the ledger into the attributed summary. `now_day` anchors the
/// anomaly's rolling window; the counts themselves are all-time.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn attention_summary(state: &ProjectState, now_day: &str) -> AttentionSummary {
    let mut by_area: BTreeMap<String, AttentionRow> = BTreeMap::new();
    let mut total = 0_u64;
    let mut unattributed = 0_u64;
    for rec in deduped(state) {
        total += 1;
        match rec.area {
            Some(area) => {
                let row = by_area
                    .entry(area.key().to_owned())
                    .or_insert_with(zero_row);
                *row.entry(rec.kind.key().to_owned()).or_insert(0) += 1;
            }
            None => unattributed += 1,
        }
    }
    for area in [TicketType::Feature, TicketType::Bug, TicketType::Chore] {
        by_area
            .entry(area.key().to_owned())
            .or_insert_with(zero_row);
    }
    let anomaly = detect_attention_anomaly(state, now_day);
    AttentionSummary {
        attention_by_area: by_area,
        interventions_total: total,
        unattributed,
        anomaly,
    }
}

/// A full zero row, one slot per intervention kind.
fn zero_row() -> AttentionRow {
    InterventionKind::ALL
        .iter()
        .map(|k| (k.key().to_owned(), 0_u64))
        .collect()
}

/// The AC3 anomaly: an area whose recent cumulative attributed attention
/// exceeds `N` standard deviations above its trailing daily average while it
/// closed zero verified tickets in the window — attention burning without
/// delivered value, the signal to tune the gate instead of enforcing it.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn detect_attention_anomaly(state: &ProjectState, now_day: &str) -> Option<AttentionAnomaly> {
    let window_start = crate::metrics::days_back(now_day, (ATTENTION_WINDOW_DAYS - 1) as u64);
    let verified_in_window = |area: TicketType| -> u64 {
        state
            .outcome_ledger
            .iter()
            .filter(|e| {
                let day = day_of(&e.verified_at);
                day.as_str() >= window_start.as_str() && day.as_str() <= now_day
            })
            .filter(|e| ticket_class_of_id(e.ticket.as_str()) == Some(area))
            .count() as u64
    };
    let mut best: Option<AttentionAnomaly> = None;
    for area in [TicketType::Feature, TicketType::Bug, TicketType::Chore] {
        let daily = daily_counts(state, now_day, area);
        let recent_start = daily.len().saturating_sub(RECENT_DAYS);
        // On a full window the head is exactly the baseline.
        debug_assert_eq!(recent_start, daily.len().min(BASELINE_DAYS));
        let (baseline, recent) = daily.split_at(recent_start);
        let recent_total: u64 = recent.iter().sum();
        if recent_total < MIN_RECENT_INTERVENTIONS {
            continue;
        }
        let n = baseline.len() as f64;
        let mean = baseline.iter().sum::<u64>() as f64 / n;
        // A baseline with no attention at all has no trailing average to
        // exceed — a project's first burst of gates is how work starts, not
        // an anomaly (and early on, "no verified tickets yet" is normal).
        if mean <= 0.0 {
            continue;
        }
        let variance = baseline
            .iter()
            .map(|v| {
                let d = *v as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / n;
        let stddev = variance.sqrt();
        let threshold = mean + ANOMALY_SIGMA * stddev;
        if (recent_total as f64) <= threshold {
            continue;
        }
        let verified = verified_in_window(area);
        if verified > 0 {
            continue; // the attention bought value — not the anomaly to flag
        }
        let candidate = AttentionAnomaly {
            area: area.key().to_owned(),
            recent_interventions: recent_total,
            baseline_mean: mean,
            baseline_stddev: stddev,
            sigma_threshold: threshold,
            verified_in_window: verified,
        };
        // Most concentrated burst wins; fixed area order breaks ties.
        let better = match &best {
            None => true,
            Some(b) => candidate.recent_interventions > b.recent_interventions,
        };
        if better {
            best = Some(candidate);
        }
    }
    best
}

/// One area's daily decision counts across the window, oldest first.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn daily_counts(state: &ProjectState, now_day: &str, area: TicketType) -> Vec<u64> {
    let mut out = vec![0_u64; ATTENTION_WINDOW_DAYS];
    for rec in deduped(state) {
        if rec.area != Some(area) {
            continue;
        }
        let day = day_of(&rec.at);
        // Walk the window from oldest to today; the first matching label wins.
        for (i, offset) in (0..ATTENTION_WINDOW_DAYS).rev().enumerate() {
            if crate::metrics::days_back(now_day, offset as u64) == day {
                out[i] += 1;
                break;
            }
        }
    }
    out
}

/// Day-by-day attention series over the rolling window — the same shape and
/// call signature as [`crate::metrics_health::compute_trends`], with month and
/// year boundaries handled by the same `days_back` civil-calendar walk and
/// every empty window day returned as an explicit zero.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn compute_attention_trends(
    state: &ProjectState,
    days: usize,
    now_day: &str,
) -> AttentionTrendsResponse {
    let records = deduped(state);
    if records.is_empty() {
        return AttentionTrendsResponse {
            insufficient_data: true,
            points: Vec::new(),
        };
    }
    let span = days.max(1);
    let mut by_day: BTreeMap<String, u64> = BTreeMap::new();
    let mut unattr_by_day: BTreeMap<String, u64> = BTreeMap::new();
    for rec in records {
        let d = day_of(&rec.at);
        *by_day.entry(d.clone()).or_insert(0) += 1;
        if rec.area.is_none() {
            *unattr_by_day.entry(d).or_insert(0) += 1;
        }
    }
    // Oldest-first labels, exactly like compute_trends.
    let points = (0..span)
        .rev()
        .map(|i| {
            let day = crate::metrics::days_back(now_day, i as u64);
            AttentionTrendPoint {
                interventions: by_day.get(&day).copied().unwrap_or(0) as f64,
                unattributed: unattr_by_day.get(&day).copied().unwrap_or(0) as f64,
                day,
            }
        })
        .collect();
    AttentionTrendsResponse {
        insufficient_data: false,
        points,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::InterventionRecord;
    use coxagent_domain::TicketId;

    /// Write a record directly with a fixed timestamp — the analytics are
    /// pure over persisted records, so tests pin time instead of calling now.
    fn rec(kind: InterventionKind, area: Option<TicketType>, at: &str) -> InterventionRecord {
        InterventionRecord {
            kind,
            ticket: String::new(),
            area,
            by: "po".into(),
            at: at.to_owned(),
        }
    }

    fn push(s: &mut ProjectState, r: InterventionRecord) {
        s.governance_interventions.push(r);
    }

    #[test]
    fn empty_ledger_is_a_zero_grid_with_no_anomaly() {
        let s = ProjectState::default();
        let sum = attention_summary(&s, "2026-08-01");
        assert_eq!(sum.interventions_total, 0);
        assert_eq!(sum.unattributed, 0);
        assert_eq!(sum.attention_by_area.len(), 3);
        for row in sum.attention_by_area.values() {
            assert_eq!(row.len(), InterventionKind::ALL.len());
            assert!(row.values().all(|v| *v == 0));
        }
        assert!(sum.anomaly.is_none());
        let tr = compute_attention_trends(&s, 14, "2026-08-01");
        assert!(tr.insufficient_data);
        assert!(tr.points.is_empty());
    }

    #[test]
    fn mixed_classes_aggregate_per_area_and_kind() {
        let mut s = ProjectState::default();
        push(
            &mut s,
            rec(
                InterventionKind::ReadyApprove,
                Some(TicketType::Feature),
                "2026-08-01T10:00:00Z",
            ),
        );
        push(
            &mut s,
            rec(
                InterventionKind::VerifySendBack,
                Some(TicketType::Bug),
                "2026-08-01T11:00:00Z",
            ),
        );
        push(
            &mut s,
            rec(
                InterventionKind::VerifyPass,
                Some(TicketType::Bug),
                "2026-08-02T11:00:00Z",
            ),
        );
        let sum = attention_summary(&s, "2026-08-14");
        assert_eq!(sum.interventions_total, 3);
        assert_eq!(sum.unattributed, 0);
        // Attributed volume is the per-area rows' sum.
        assert_eq!(
            sum.attention_by_area
                .values()
                .map(|r| r.values().sum::<u64>())
                .sum::<u64>(),
            3
        );
        assert_eq!(
            sum.attention_by_area["feature"]
                .get("ready_approve")
                .copied(),
            Some(1)
        );
        assert_eq!(
            sum.attention_by_area["bug"]
                .get("verify_send_back")
                .copied(),
            Some(1)
        );
        assert_eq!(
            sum.attention_by_area["bug"].get("verify_pass").copied(),
            Some(1)
        );
        assert_eq!(sum.attention_by_area["chore"].values().sum::<u64>(), 0);
    }

    #[test]
    fn unattributed_records_are_reported_explicitly_not_dropped() {
        // AC4: no derivable area → the explicit bucket, still in the total.
        let mut s = ProjectState::default();
        push(
            &mut s,
            rec(
                InterventionKind::HumanPrDismissed,
                None,
                "2026-08-01T10:00:00Z",
            ),
        );
        let sum = attention_summary(&s, "2026-08-14");
        assert_eq!(sum.interventions_total, 1);
        assert_eq!(sum.unattributed, 1);
        // Nothing attributed: the per-area rows are all zero.
        assert!(sum
            .attention_by_area
            .values()
            .all(|r| r.values().all(|v| *v == 0)));
    }

    #[test]
    fn a_concurrent_double_write_of_one_resolution_counts_once() {
        // AC5: the same decision double-written by a lost-update race
        // (identical kind/ticket/operator, same second) aggregates once; a
        // genuine re-decision later keeps counting.
        let mut s = ProjectState::default();
        let decision = || InterventionRecord {
            kind: InterventionKind::HumanPrDismissed,
            ticket: "CXC-F001".into(),
            area: Some(TicketType::Feature),
            by: "po".into(),
            at: "2026-08-01T10:00:00.482193Z".into(),
        };
        push(&mut s, decision());
        push(&mut s, decision());
        let sum = attention_summary(&s, "2026-08-14");
        assert_eq!(sum.interventions_total, 1);
        assert_eq!(
            sum.attention_by_area["feature"]
                .get("human_pr_dismissed")
                .copied(),
            Some(1)
        );
        // A later re-decision of the same item is a second decision.
        let mut later = decision();
        later.at = "2026-08-01T10:05:00Z".into();
        push(&mut s, later);
        assert_eq!(attention_summary(&s, "2026-08-14").interventions_total, 2);
    }

    #[test]
    fn trends_zero_fill_empty_days_and_cross_month_and_year_boundaries() {
        // AC2: same signature/walk as compute_trends; a decision on Jan 1
        // trends into zeroes across the December→January boundary.
        let mut s = ProjectState::default();
        push(
            &mut s,
            rec(
                InterventionKind::VerifyPass,
                Some(TicketType::Bug),
                "2026-01-01T00:00:00Z",
            ),
        );
        let tr = compute_attention_trends(&s, 5, "2026-01-03");
        assert!(!tr.insufficient_data);
        assert_eq!(tr.points.len(), 5);
        assert_eq!(tr.points[0].day, "2025-12-30");
        assert!(tr.points[0].interventions.abs() < f64::EPSILON);
        let jan1 = tr
            .points
            .iter()
            .find(|p| p.day == "2026-01-01")
            .expect("jan1");
        assert!((jan1.interventions - 1.0).abs() < f64::EPSILON);
        // Empty days within a recorded project are explicit zeroes.
        assert_eq!(
            tr.points
                .iter()
                .filter(|p| p.interventions.abs() < f64::EPSILON)
                .count(),
            4
        );
    }

    #[test]
    fn anomaly_fires_on_a_spike_with_flat_delivered_value() {
        // AC3: 11 baseline days at 0–1 decisions, then 4 in the trailing 3
        // days, and no verified tickets of that class in the window.
        let mut s = ProjectState::default();
        for d in 1..=11 {
            push(
                &mut s,
                rec(
                    InterventionKind::ReadyApprove,
                    Some(TicketType::Feature),
                    &format!("2026-07-{d:02}T10:00:00Z"),
                ),
            );
        }
        for d in [12, 13, 14] {
            push(
                &mut s,
                rec(
                    InterventionKind::VerifySendBack,
                    Some(TicketType::Feature),
                    &format!("2026-07-{d:02}T10:00:00Z"),
                ),
            );
            push(
                &mut s,
                rec(
                    InterventionKind::VerifySendBack,
                    Some(TicketType::Feature),
                    &format!("2026-07-{d:02}T11:00:00Z"),
                ),
            );
        }
        let a = detect_attention_anomaly(&s, "2026-07-14").expect("anomaly");
        assert_eq!(a.area, "feature");
        assert_eq!(a.recent_interventions, 6);
        assert_eq!(a.verified_in_window, 0);
    }

    #[test]
    fn anomaly_stays_silent_when_the_attention_bought_value() {
        let mut s = ProjectState::default();
        for d in 1..=11 {
            push(
                &mut s,
                rec(
                    InterventionKind::ReadyApprove,
                    Some(TicketType::Bug),
                    &format!("2026-07-{d:02}T10:00:00Z"),
                ),
            );
        }
        for d in [12, 13, 14] {
            push(
                &mut s,
                rec(
                    InterventionKind::VerifySendBack,
                    Some(TicketType::Bug),
                    &format!("2026-07-{d:02}T10:00:00Z"),
                ),
            );
            push(
                &mut s,
                rec(
                    InterventionKind::VerifySendBack,
                    Some(TicketType::Bug),
                    &format!("2026-07-{d:02}T11:00:00Z"),
                ),
            );
        }
        // Delivered value: a verified bug inside the window (outcome ledger).
        s.outcome_ledger.push(crate::state::OutcomeLedgerEntry {
            ticket: TicketId::new("CXC-B001").expect("id"),
            goal: None,
            capture_commit: None,
            verified_at: "2026-07-13T00:00:00Z".into(),
        });
        assert!(detect_attention_anomaly(&s, "2026-07-14").is_none());
    }

    #[test]
    fn a_first_small_burst_is_noise_not_an_anomaly() {
        let mut s = ProjectState::default();
        for d in [1, 2, 3] {
            push(
                &mut s,
                rec(
                    InterventionKind::ReadyApprove,
                    Some(TicketType::Chore),
                    &format!("2026-08-{d:02}T10:00:00Z"),
                ),
            );
        }
        assert!(detect_attention_anomaly(&s, "2026-08-03").is_none());
    }
}
