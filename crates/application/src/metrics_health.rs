//! Cycle-performance health overlay (CXA-F018). Pure analytics over ProjectState.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::state::ProjectState;

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct SprintVelocity {
    pub sprint: u32,
    pub committed: usize,
    pub done: usize,
}

/// One calendar day metered spend on the cycle-scorecard ledger.
#[derive(Debug, Serialize, Clone)]
pub struct DailySpend {
    pub day: String,
    /// Number of cycles whose scorecard landed on this day.
    pub cycles: usize,
    /// Sum of those cycles USD cost this day.
    pub cost_usd: f64,
}

/// Delivery summary across recorded work.
#[derive(Debug, Serialize, Clone)]
pub struct PerSprintSummary {
    /// Tickets deployed in total (history.len()). Never negative.
    pub shipped_total: usize,
}

/// Burn-rate rollup across completed cycles plus overall win rate.
#[derive(Debug, Serialize, Clone)]
pub struct BurnRate {
    /// Fraction of completed cycles that shipped at least one ticket (0-1).
    pub win_rate: f64,
    /// Lifetime metered USD spend.
    pub total_cost_usd: f64,
}

/// Cost split between FEATURE/BUG outcomes from role-key attribution.
#[derive(Debug, Serialize, Clone)]
pub struct CostByOutcomeType {
    /// Spend attributed to roles whose key names feature work (`dev_feature`).
    pub feature_usd: f64,
    /// Spend attributed to roles whose key names bug work (`dev_bug`, ...).
    pub bug_usd: f64,
}

/// A single failing gate ranked by how often it rejected work.
#[derive(Debug, Serialize)]
pub struct GateCount {
    pub gate: String,
    pub count: usize,
}

/// A ticket that failed repeatedly, for spotting recurring problem work.
#[derive(Debug, Serialize)]
pub struct TicketRecurrence {
    pub id: String,
    pub fail_count: u32,
}

/// Failure-mode recognition from each ticket's attempt-failure log.
#[derive(Debug, Serialize)]
pub struct PatternSummary {
    /// Most common failing gates ranked by frequency (top 3).
    pub most_failed_gates_top3: Vec<GateCount>,
    /// How failures split across layers (`spec`, `gate`, `design`, `infra`).
    pub failure_layer_dist: BTreeMap<String, usize>,
    /// Tickets with two or more recorded failures (top 5 by count).
    pub recurring_tickets_top5: Vec<TicketRecurrence>,
    /// Total structured attempt-failures recorded across every ticket.
    pub total_failures: usize,
}

/// A burn-rate anomaly on one completed cycle vs its sprint average.
#[derive(Debug, Serialize)]
pub struct BurnWarning {
    /// Calendar day of the anomalous cycle scorecard.
    pub day: String,
    /// That cycle's USD cost.
    pub cost_usd: f64,
    /// Mean cost of the last up-to-5 completed cycles compared against.
    pub avg_cost_usd: f64,
}

/// The full cycle-performance health overlay for the dashboard Overview panel.
#[derive(Debug, Serialize)]
pub struct CyclePerfSummary {
    pub velocity_by_sprint: Vec<SprintVelocity>,
    pub per_sprint_summary: PerSprintSummary,
    pub burn_rate: BurnRate,
    /// Spend bucketed by calendar day across completed cycles.
    pub daily_spend_last14d: Vec<DailySpend>,
    pub cost_by_outcome_type: CostByOutcomeType,
    pub patterns: PatternSummary,
    /// Deploys in the last 7 days.
    pub deploy_7d: usize,
    /// Set when one recent cycle's cost exceeds 2x its sprint average (AC4).
    pub burn_warning: Option<BurnWarning>,
}

/// One day of a time-series trend (line-chart point).
#[derive(Debug, Serialize)]
pub struct TrendPoint {
    pub day: String,
    /// Tickets deployed that day.
    pub velocity: f64,
    /// USD spent that day across all phases.
    pub spend_usd: f64,
}

/// Time-series response for /metrics/trends (AC3 + AC5 insufficient-data edge).
#[derive(Debug, Serialize)]
pub struct CycleTrendsResponse {
    /// True when fewer than three completed cycles exist; points is then empty
    /// and the UI renders "(insufficient data)" instead of charts.
    pub insufficient_data: bool,
    /// Day-by-day series oldest-first; empty when no timestamped source exists.
    pub points: Vec<TrendPoint>,
}

/// Calendar day (`YYYY-MM-DD`) from an RFC3339 timestamp; best-effort fallback.
fn day_of(at: &str) -> String {
    at.split('T').next().unwrap_or(at).to_owned()
}

/// Number of completed cycles recorded on the scorecard ledger.
#[must_use]
pub fn completed_cycles(state: &ProjectState) -> usize {
    state.cycle_scores.len()
}

/// Closed sprints plus the running sprint as committed-vs-done rows.
#[must_use]
pub fn compute_velocity(state: &ProjectState) -> Vec<SprintVelocity> {
    let mut out: Vec<SprintVelocity> = state
        .sprints
        .iter()
        .map(|r| SprintVelocity {
            sprint: r.number,
            committed: r.committed,
            done: r.done,
        })
        .collect();
    if let Some(sp) = &state.sprint {
        let done = sp
            .committed
            .iter()
            .filter(|cid| {
                state.tickets.iter().any(|t| {
                    t.id().as_str() == cid.as_str()
                        && matches!(
                            t.status(),
                            coxagent_domain::Status::Done
                                | coxagent_domain::Status::Documented
                                | coxagent_domain::Status::Verified
                        )
                })
            })
            .count();
        out.push(SprintVelocity {
            sprint: sp.number,
            committed: sp.committed.len(),
            done,
        });
    }
    out.sort_by_key(|v| v.sprint);
    out
}

/// Burn-rate rollup: overall win rate + lifetime spend, plus a day-by-day
/// spend series across completed cycles.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
#[must_use]
pub fn compute_burn(state: &ProjectState) -> (BurnRate, Vec<DailySpend>) {
    let total = state.cycle_scores.len();
    let wins = state.cycle_scores.iter().filter(|c| c.shipped > 0).count();
    let win_rate = if total == 0 {
        0.0
    } else {
        wins as f64 / total as f64
    };
    // Bucket per-cycle scorecards by calendar day, keep newest 14 days.
    let mut by_day: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    for c in &state.cycle_scores {
        let d = day_of(&c.at);
        let e = by_day.entry(d).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += c.cost_usd;
    }
    let daily: Vec<DailySpend> = by_day
        .iter()
        .skip(by_day.len().saturating_sub(14))
        .map(|(day, (cycles, cost))| DailySpend {
            day: day.clone(),
            cycles: *cycles,
            cost_usd: *cost,
        })
        .collect();
    (
        BurnRate {
            win_rate,
            total_cost_usd: state.spend.total_cost_usd,
        },
        daily,
    )
}

/// Cost attributed to FEATURE vs BUG outcomes from role-key names.
#[must_use]
pub fn compute_cost_by_outcome(state: &ProjectState) -> CostByOutcomeType {
    let mut feature_usd = 0.0_f64;
    let mut bug_usd = 0.0_f64;
    for (role, cost) in &state.spend.by_role {
        if role.contains("feature") {
            feature_usd += cost;
        } else if role.contains("bug") {
            bug_usd += cost;
        }
    }
    CostByOutcomeType {
        feature_usd,
        bug_usd,
    }
}

/// Bound a collection length into a u32 without lossy truncation errors.
fn to_u32_safe(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Human label for a failure layer (stable across persistence/serde).
fn layer_label(layer: crate::state::FailureLayer) -> &'static str {
    match layer {
        crate::state::FailureLayer::Spec => "spec",
        crate::state::FailureLayer::Gate => "gate",
        crate::state::FailureLayer::Design => "design",
        crate::state::FailureLayer::Infra => "infra",
    }
}

/// Failure-mode recognition from the structured per-ticket failure log.
#[must_use]
pub fn compute_patterns(state: &ProjectState) -> PatternSummary {
    let mut gates: BTreeMap<String, usize> = BTreeMap::new();
    let mut layers: BTreeMap<String, usize> = BTreeMap::new();
    for failures in state.ticket_failures.values() {
        for f in failures {
            *gates.entry(f.gate.clone()).or_insert(0) += 1;
            *layers.entry(layer_label(f.layer).to_owned()).or_insert(0) += 1;
        }
    }
    let mut gate_vec: Vec<(String, usize)> = gates.into_iter().collect();
    gate_vec.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let most_failed_gates_top3 = gate_vec
        .into_iter()
        .take(3)
        .map(|(gate, count)| GateCount { gate, count })
        .collect();

    // Recurring tickets are those that failed two or more times; rank by count
    // descending then id ascending for a deterministic order.
    let mut recurring: Vec<(String, u32)> = state
        .ticket_failures
        .iter()
        .filter_map(|(id, fs)| {
            let n = to_u32_safe(fs.len());
            if n >= 2 {
                Some((id.clone(), n))
            } else {
                None
            }
        })
        .collect();
    recurring.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let recurring_tickets_top5 = recurring
        .into_iter()
        .take(5)
        .map(|(id, fail_count)| TicketRecurrence { id, fail_count })
        .collect();

    let total_failures = state
        .ticket_failures
        .values()
        .fold(0usize, |acc, fs| acc + fs.len());
    PatternSummary {
        most_failed_gates_top3,
        failure_layer_dist: layers,
        recurring_tickets_top5,
        total_failures,
    }
}

/// AC4 anomaly check: if any of the last up-to-5 completed cycles cost more
/// than 2x the mean of that window, surface it as a burn-rate warning.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn detect_burn_warning(state: &ProjectState) -> Option<BurnWarning> {
    let recent = state.cycle_scores.iter().rev().take(5).collect::<Vec<_>>();
    if recent.len() < 2 {
        return None;
    }
    let avg = recent.iter().map(|c| c.cost_usd).sum::<f64>() / recent.len() as f64;
    if avg <= 0.0 {
        return None;
    }
    // Highest cost in the window that clears the threshold (last-write wins on
    // ties so the most recent anomaly is named).
    let mut found: Option<BurnWarning> = None;
    for c in &recent {
        if c.cost_usd > 2.0 * avg {
            found = Some(BurnWarning {
                day: day_of(&c.at),
                cost_usd: c.cost_usd,
                avg_cost_usd: avg,
            });
        }
    }
    found
}

/// Full health overlay for the Overview panel. `now_day` is `YYYY-MM-DD` and is
/// injected so tests are deterministic (the endpoint passes today's UTC date).
#[must_use]
pub fn compute_cycle_perf(state: &ProjectState, now_day: &str) -> CyclePerfSummary {
    let velocity_by_sprint = compute_velocity(state);
    let per_sprint_summary = PerSprintSummary {
        shipped_total: state.history.len(),
    };
    let (burn_rate, daily_spend_last14d) = compute_burn(state);
    let cost_by_outcome_type = compute_cost_by_outcome(state);
    let patterns = compute_patterns(state);
    let week_ago = crate::metrics::days_back(now_day, 7);
    let deploy_7d = state
        .history
        .iter()
        .filter(|r| day_of(&r.at) >= week_ago)
        .count();
    CyclePerfSummary {
        velocity_by_sprint,
        per_sprint_summary,
        burn_rate,
        daily_spend_last14d,
        cost_by_outcome_type,
        patterns,
        deploy_7d,
        burn_warning: detect_burn_warning(state),
    }
}

/// Day-by-day time-series for line charts (AC3), with the AC5 insufficient-data
/// edge: fewer than three completed cycles yields an empty series flagged so the
/// UI renders "(insufficient data)" rather than a misleading chart.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
#[must_use]
pub fn compute_trends(state: &ProjectState, days: usize, now_day: &str) -> CycleTrendsResponse {
    let insufficient_data = completed_cycles(state) < 3;
    if insufficient_data {
        return CycleTrendsResponse {
            insufficient_data: true,
            points: Vec::new(),
        };
    }
    let span = days.max(1);
    let mut deploy_by_day: BTreeMap<String, f64> = BTreeMap::new();
    for r in &state.history {
        *deploy_by_day.entry(day_of(&r.at)).or_insert(0.0) += 1.0;
    }
    let mut spend_by_day: BTreeMap<String, f64> = BTreeMap::new();
    for c in &state.cycle_scores {
        *spend_by_day.entry(day_of(&c.at)).or_insert(0.0) += c.cost_usd;
    }
    // Walk backward from today producing oldest-first labels so charts are in
    // ascending time order without relying on insertion order.
    let mut labels = Vec::with_capacity(span);
    for i in (0..span).rev() {
        labels.push(crate::metrics::days_back(now_day, i as u64));
    }
    let points = labels
        .into_iter()
        .map(|day| TrendPoint {
            velocity: deploy_by_day.get(&day).copied().unwrap_or(0.0),
            spend_usd: spend_by_day.get(&day).copied().unwrap_or(0.0),
            day,
        })
        .collect();
    CycleTrendsResponse {
        insufficient_data: false,
        points,
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::state::{CycleScore, DeployRecord, SprintRecord};
    use coxagent_domain::{SemVer, TicketId};

    fn ticket(id: &str) -> coxagent_domain::Ticket {
        let json = serde_json::json!({
            "id": id,
            "type": "feature",
            "title": "t",
            "description": "",
            "priority": "medium",
            "complexity": "small",
            "status": "done",
            "has_ui": false,
            "design": {"technical": null, "ux": null},
            "parent_id": null,
            "depends_on": []
        });
        serde_json::from_value(json).expect("ticket")
    }

    fn cycle(n: u64, at: &str, cost: f64, shipped: u64) -> CycleScore {
        CycleScore {
            cycle: n,
            at: at.to_owned(),
            runs: 1,
            useful: 1,
            cost_usd: cost,
            shipped,
            incidents: 0,
            errors: 0,
            grade: "B".to_owned(),
            ..CycleScore::default()
        }
    }

    #[test]
    fn empty_state_is_all_zeroes_and_no_warning() {
        let s = ProjectState::default();
        let sum = compute_cycle_perf(&s, "2026-08-01");
        assert_eq!(sum.velocity_by_sprint.len(), 0);
        assert_eq!(sum.per_sprint_summary.shipped_total, 0);
        assert!((sum.burn_rate.win_rate).abs() < f64::EPSILON);
        assert!(sum.burn_warning.is_none());
        let tr = compute_trends(&s, 14, "2026-08-01");
        assert!(tr.insufficient_data);
        assert!(tr.points.is_empty());
    }

    #[test]
    fn velocity_comes_from_closed_and_running_sprints() {
        let mut s = ProjectState::default();
        s.sprints = vec![
            SprintRecord {
                number: 1,
                goal: "g".into(),
                committed: 8,
                done: 6,
                at: "2026-07-01T00:00:00Z".into(),
            },
            SprintRecord {
                number: 2,
                goal: "g".into(),
                committed: 8,
                done: 7,
                at: "2026-07-08T00:00:00Z".into(),
            },
        ];
        let v = compute_velocity(&s);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].committed, 8);
        assert_eq!(v[0].done, 6);
    }

    #[test]
    fn burn_tracks_win_rate_and_daily_spend() {
        let mut s = ProjectState::default();
        s.spend.total_cost_usd = 5.0;
        s.cycle_scores = vec![
            cycle(1, "2026-07-29T00:00:00Z", 1.0, 1),
            cycle(2, "2026-07-30T00:00:00Z", 1.0, 0),
            cycle(3, "2026-07-31T00:00:00Z", 2.0, 1),
            cycle(4, "2026-08-01T00:00:00Z", 3.0, 1),
            cycle(5, "2026-08-01T10:00:00Z", 4.0, 1),
        ];
        let (burn, daily) = compute_burn(&s);
        assert!((burn.win_rate - (4.0 / 5.0)).abs() < f64::EPSILON);
        assert!((burn.total_cost_usd - 5.0).abs() < f64::EPSILON);
        // Two scorecards share the same calendar day and merge into one row.
        let aug1 = daily.iter().find(|d| d.day == "2026-08-01").expect("aug1");
        assert!((aug1.cost_usd - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_by_outcome_splits_on_role_key() {
        let mut s = ProjectState::default();
        s.spend.by_role.insert("dev_feature".into(), 2.5);
        s.spend.by_role.insert("dev_bug".into(), 3.5);
        s.spend.by_role.insert("ba".into(), 9.9);
        let c = compute_cost_by_outcome(&s);
        assert!((c.feature_usd - 2.5).abs() < f64::EPSILON);
        assert!((c.bug_usd - 3.5).abs() < f64::EPSILON);
    }

    fn failure(attempt: u32, gate: &str) -> crate::state::AttemptFailure {
        crate::state::AttemptFailure {
            attempt,
            layer: crate::state::FailureLayer::Gate,
            gate: gate.to_owned(),
            detail: "d".into(),
            files: vec![],
        }
    }

    #[test]
    fn patterns_rank_gates_and_recurring_tickets() {
        let mut s = ProjectState::default();
        s.ticket_failures.insert(
            "A-1".into(),
            vec![failure(1, "clippy"), failure(2, "clippy")],
        );
        s.ticket_failures
            .insert("A-2".into(), vec![failure(1, "tests")]);
        let p = compute_patterns(&s);
        assert_eq!(p.most_failed_gates_top3[0].gate, "clippy");
        assert_eq!(p.most_failed_gates_top3[0].count, 2);
        assert_eq!(p.failure_layer_dist.get("gate").copied().unwrap_or(0), 3);
        assert_eq!(p.recurring_tickets_top5.len(), 1);
        assert_eq!(p.recurring_tickets_top5[0].id, "A-1");
    }

    #[test]
    fn burn_warning_fires_when_one_cycle_exceeds_twice_the_average() {
        let mut s = ProjectState::default();
        s.cycle_scores
            .push(cycle(1, "2026-08-01T00:00:00Z", 1.0, 0));
        s.cycle_scores
            .push(cycle(2, "2026-08-02T00:00:00Z", 1.0, 0));
        s.cycle_scores
            .push(cycle(3, "2026-08-03T00:00:00Z", 1.0, 0));
        s.cycle_scores
            .push(cycle(4, "2026-08-04T00:00:00Z", 10.0, 1));
        let w = detect_burn_warning(&s).expect("warning");
        assert_eq!(w.day, "2026-08-04");
        assert!((w.cost_usd - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn trends_need_three_completed_cycles() {
        let mut s = ProjectState::default();
        s.history.push(DeployRecord {
            version: SemVer::new(1, 0, 0),
            ticket: TicketId::new("F001").expect("id"),
            title: "t".into(),
            at: "2026-07-30T12:00:00Z".into(),
        });
        // Only two completed cycles -> insufficient data.
        s.cycle_scores
            .push(cycle(1, "2026-07-29T00:00:00Z", 1.0, 1));
        s.cycle_scores
            .push(cycle(2, "2026-07-30T00:00:00Z", 1.0, 0));
        let tr = compute_trends(&s, 14, "2026-08-01");
        assert!(tr.insufficient_data);
    }
    #[test]
    fn velocity_counts_done_tickets_in_the_running_sprint() {
        let mut s = ProjectState::default();
        let f1 = TicketId::new("F001").expect("id");
        s.sprint = Some(crate::state::Sprint {
            number: 3,
            goal: "g".into(),
            started_cycle: 1,
            length_cycles: 5,
            committed: vec![f1.clone()],
            started_at: "2026-07-01T00:00:00Z".into(),
        });
        s.tickets.push(ticket("F001"));
        let v = compute_velocity(&s);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].committed, 1);
        assert_eq!(v[0].done, 1);
    }

    #[test]
    fn trends_build_a_day_series_after_three_cycles() {
        let mut s = ProjectState::default();
        for n in [1u64, 2u64, 3u64] {
            s.cycle_scores.push(cycle(n, "2026-07-30T00:00:00Z", 2.0, 1));
        }
        s.history.push(DeployRecord {
            version: SemVer::new(1, 0, 0),
            ticket: TicketId::new("F001").expect("id"),
            title: "t".into(),
            at: "2026-07-30T12:00:00Z".into(),
        });
        let tr = compute_trends(&s, 5, "2026-08-03");
        assert!(!tr.insufficient_data);
        let d = tr.points.iter().find(|p| p.day == "2026-07-30").expect("day");
        assert!((d.spend_usd - 6.0).abs() < f64::EPSILON);
        assert!((d.velocity - 1.0).abs() < f64::EPSILON);
    }
}
