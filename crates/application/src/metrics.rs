//! Team metrics — computed purely from state so the dashboard, reports, and the
//! eventual SM retro all read the same numbers. No IO, fully testable.

use crate::state::ProjectState;
use coxagent_domain::{Status, TicketType};
use serde::Serialize;
use std::collections::BTreeMap;

/// A snapshot of team health derived from the project state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metrics {
    pub version: String,
    pub total_tickets: usize,
    pub by_status: BTreeMap<String, usize>,
    pub features_shipped: usize,
    pub features_in_flight: usize,
    pub bugs_open: usize,
    pub bugs_verified: usize,
    pub releases: usize,
    /// Deploys per day (RFC3339 date -> count), oldest first — a burndown source.
    pub deploys_by_day: Vec<DayCount>,
    /// Share of shipped work that was features vs bug fixes (0-100).
    pub feature_ratio_pct: u32,
    /// The human governance-attention ledger, aggregated (CXA-F230) — the
    /// operator-side mirror of the agent-side counts above. Additive; old
    /// clients that never read the key are unaffected.
    pub attention: crate::metrics_governance::AttentionSummary,
}

/// Per-role performance snapshot for the Agents "evals" panel — all
/// deterministic state math, zero tokens.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoleEval {
    pub role: String,
    pub runs: u64,
    pub cost_usd: f64,
    pub avg_cost_usd: f64,
}

/// Team quality/efficiency evals computed from persisted state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentEvals {
    pub per_role: Vec<RoleEval>,
    /// Tickets shipped (deploy history length).
    pub shipped_total: usize,
    /// Shipped in the last 7 days.
    pub shipped_7d: usize,
    /// Tickets currently parked (3 failed attempts, waiting on a human).
    pub parked: usize,
    /// Failed attempts recorded across all tickets — retry churn indicator.
    pub failed_attempts: u64,
    /// Failed attempts per shipped ticket (churn per unit of delivery).
    pub churn_per_ship: f64,
    /// PRs currently stuck in the fix ladder (2+ fix rounds).
    pub prs_stuck: usize,
    /// Cost per shipped ticket (total spend / shipped).
    pub cost_per_ship_usd: f64,
}

/// Compute the evals panel from state. Pure.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn agent_evals(state: &ProjectState) -> AgentEvals {
    let per_role: Vec<RoleEval> = state
        .spend
        .by_role
        .iter()
        .map(|(role, cost)| {
            let runs = state.spend.runs_by_role.get(role).copied().unwrap_or(0);
            let metered = state
                .spend
                .metered_cost_by_role
                .get(role)
                .copied()
                .unwrap_or(0.0);
            RoleEval {
                role: role.clone(),
                runs,
                cost_usd: *cost,
                avg_cost_usd: if runs == 0 {
                    0.0
                } else {
                    metered / runs as f64
                },
            }
        })
        .collect();
    let shipped_total = state.history.len();
    let cutoff = crate::state::now_rfc3339();
    let cutoff_day = cutoff.get(..10).unwrap_or("").to_owned();
    let week_ago = days_back(&cutoff_day, 7);
    let shipped_7d = state
        .history
        .iter()
        .filter(|r| r.at.get(..10).unwrap_or("") >= week_ago.as_str())
        .count();
    let parked = state
        .ticket_fail_attempts
        .values()
        .filter(|n| **n >= 3)
        .count();
    let failed_attempts: u64 = state
        .ticket_fail_attempts
        .values()
        .map(|n| u64::from(*n))
        .sum();
    let prs_stuck = state.pr_fix_attempts.values().filter(|n| **n >= 2).count();
    AgentEvals {
        per_role,
        shipped_total,
        shipped_7d,
        parked,
        failed_attempts,
        churn_per_ship: if shipped_total == 0 {
            failed_attempts as f64
        } else {
            failed_attempts as f64 / shipped_total as f64
        },
        prs_stuck,
        cost_per_ship_usd: if shipped_total == 0 {
            0.0
        } else {
            state.spend.total_cost_usd / shipped_total as f64
        },
    }
}

/// Pure self-tuning policy: given the current evals, backlog size, and the
/// bug burn-down delta, decide the brakes. Hysteresis (on at a high bar, off
/// at a lower one) prevents flapping.
///
/// `bug_delta_24h` is the net open-bug change over the last day
/// ([`compute_burndown`]'s `delta_24h`): negative means the backlog GREW.
#[must_use]
pub fn decide_tuning(
    evals: &AgentEvals,
    backlog: usize,
    bug_delta_24h: i64,
    current: &crate::state::Tuning,
) -> crate::state::Tuning {
    let mut next = current.clone();
    // Quality brake: churn hot → bugs first; recovered → resume features.
    if evals.shipped_total >= 3 {
        if evals.churn_per_ship > 1.5 {
            next.bugs_first = true;
        } else if evals.churn_per_ship < 0.8 {
            next.bugs_first = false;
        }
    }
    // Burn-down escalation (CXA-F032): while the open-bug backlog is GROWING,
    // the quality brake holds no matter how churn looks — a growing bug pile
    // is itself the emergency, and a recovered churn number must not release
    // features while the backlog is still climbing.
    if bug_delta_24h < 0 {
        next.bugs_first = true;
    }
    // Intake brake: pause BA only when the backlog is far beyond throughput AND
    // nothing has shipped for a full week (a genuine stall that risks piling
    // more work onto an un-drainable queue). The moment any feature ships the
    // pipeline is healthy — release the brake so BA keeps feeding work and the
    // backlog drains. Parking BA on a big backlog is what makes it never drain.
    if evals.shipped_7d == 0 {
        if backlog > 25 {
            next.skip_ba = true;
        } else if backlog < 12 {
            next.skip_ba = false;
        }
    } else if next.skip_ba {
        next.skip_ba = false;
    }
    next
}

/// `YYYY-MM-DD` minus `n` days (lexicographic-comparable). Falls back to the
/// input on parse trouble — fine for a dashboard stat.
#[allow(clippy::many_single_char_names)] // civil-calendar math keeps the canonical y/m/d notation
pub(crate) fn days_back(day: &str, n: u64) -> String {
    let parse = |s: &str| -> Option<(i64, i64, i64)> {
        let mut it = s.split('-');
        Some((
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
        ))
    };
    let Some((y, m, d)) = parse(day) else {
        return day.to_owned();
    };
    // Days-from-civil (Howard Hinnant) and back — exact, no time dep.
    let civil = |y: i64, m: i64, d: i64| -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    };
    let from_civil = |z: i64| -> (i64, i64, i64) {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (if m <= 2 { y + 1 } else { y }, m, d)
    };
    let (y2, m2, d2) = from_civil(civil(y, m, d) - i64::try_from(n).unwrap_or(0));
    format!("{y2:04}-{m2:02}-{d2:02}")
}

/// A single day's deploy count for the activity sparkline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DayCount {
    pub day: String,
    pub count: usize,
}

/// Compute metrics from the current state.
#[must_use]
pub fn compute(state: &ProjectState) -> Metrics {
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    let mut features_shipped = 0;
    let mut features_in_flight = 0;
    let mut bugs_open = 0;
    let mut bugs_verified = 0;

    for t in &state.tickets {
        *by_status
            .entry(status_key(t.status()).to_owned())
            .or_default() += 1;
        let is_feature = matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore);
        if is_feature && matches!(t.status(), Status::Done | Status::Documented) {
            features_shipped += 1;
        }
        if is_feature && matches!(t.status(), Status::Ready | Status::InProgress) {
            features_in_flight += 1;
        }
        if t.ticket_type() == TicketType::Bug {
            match t.status() {
                Status::Open => bugs_open += 1,
                Status::Verified => bugs_verified += 1,
                _ => {}
            }
        }
    }

    let mut day_map: BTreeMap<String, usize> = BTreeMap::new();
    let mut feature_deploys: usize = 0;
    for rec in &state.history {
        let day = rec.at.split('T').next().unwrap_or(&rec.at).to_owned();
        *day_map.entry(day).or_default() += 1;
        if is_feature_id(rec.ticket.as_str()) {
            feature_deploys += 1;
        }
    }
    let deploys_by_day = day_map
        .into_iter()
        .map(|(day, count)| DayCount { day, count })
        .collect();

    let releases = state.history.len();
    let feature_ratio_pct = (feature_deploys * 100)
        .checked_div(releases)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);

    Metrics {
        version: state.current_version.to_string(),
        total_tickets: state.tickets.len(),
        by_status,
        features_shipped,
        features_in_flight,
        bugs_open,
        bugs_verified,
        releases,
        deploys_by_day,
        feature_ratio_pct,
        attention: crate::metrics_governance::attention_summary(state, &now_day()),
    }
}

/// Today's UTC calendar day — `compute` reads the clock like `agent_evals`
/// already does (the parameterized analytics live in `metrics_health`, whose
/// endpoints inject `now_day` so tests can pin time).
#[must_use]
fn now_day() -> String {
    crate::state::now_rfc3339().get(..10).unwrap_or_default().to_owned()
}

/// Whether a ticket id denotes a feature or chore. Ids are `F001` / `C001` or
/// alias-prefixed `CXC-F001`; the type code is the first char of the last
/// dash-segment.
fn is_feature_id(id: &str) -> bool {
    let seg = id.rsplit('-').next().unwrap_or(id);
    matches!(seg.chars().next(), Some('F' | 'C'))
}

// Bug burn-down analytics live in their own module (`metrics_burndown`, the
// same seam as `metrics_health`) and are re-exported here so every reader —
// the dashboard endpoints, the self-tuning pass, the tests — uses one path.
pub use crate::metrics_burndown::{
    bug_status_counts, compute_burndown, record_burndown_snapshot, Burndown, BurndownDay,
    BURNDOWN_WINDOW_DAYS,
};

fn status_key(s: Status) -> &'static str {
    match s {
        Status::Pending => "pending",
        Status::Ready => "ready",
        Status::InProgress => "in_progress",
        Status::Done => "done",
        Status::Documented => "documented",
        Status::Rejected => "rejected",
        Status::Open => "open",
        Status::Fixed => "fixed",
        Status::Verified => "verified",
        Status::OnHold => "on_hold",
    }
}

/// A one-message daily digest for the team chat: what shipped in the last 24h,
/// what it cost, where the sprint stands, and anything stuck. Pure over state.
#[must_use]
pub fn digest_markdown(state: &ProjectState, now_rfc3339: &str) -> String {
    use std::fmt::Write as _;
    // "Last 24h" by RFC3339 lexicographic compare on a prefix cut 24h back —
    // both are UTC RFC3339, so string order is time order.
    let cutoff = time::OffsetDateTime::now_utc()
        .saturating_sub(time::Duration::hours(24))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let shipped: Vec<&crate::state::DeployRecord> = state
        .history
        .iter()
        .filter(|d| d.at.as_str() > cutoff.as_str())
        .collect();
    let in_progress = state
        .tickets
        .iter()
        .filter(|t| t.status() == Status::InProgress)
        .count();
    let open_bugs = state
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
        .count();
    let mut out = format!(
        "**Daily digest** · {}\n",
        &now_rfc3339[..10.min(now_rfc3339.len())]
    );
    if shipped.is_empty() {
        out.push_str("- Shipped (24h): nothing new\n");
    } else {
        let _ = writeln!(out, "- Shipped (24h): {}", shipped.len());
        for d in shipped.iter().take(6) {
            let _ = writeln!(out, "  - {} {} — {}", d.version, d.ticket, d.title);
        }
    }
    if let Some(sp) = &state.sprint {
        let done = crate::sprint::done_count(state);
        let _ = writeln!(
            out,
            "- Sprint {}: {}/{} committed done — goal: {}",
            sp.number,
            done,
            sp.committed.len(),
            sp.goal
        );
    }
    let _ = writeln!(out, "- In flight: {in_progress} · open bugs: {open_bugs}");
    let _ = writeln!(
        out,
        "- Spend to date: ${:.2} ({} runs)",
        state.spend.total_cost_usd, state.spend.runs
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DeployRecord;
    use coxagent_domain::{SemVer, Ticket, TicketId};

    fn ticket(id: &str, ty: TicketType, status: Status) -> Ticket {
        // Build via serde to place it directly in a target status for the test.
        let json = serde_json::json!({
            "id": id, "type": ty_key(ty), "title": "t", "description": "",
            "priority": "medium", "complexity": "small", "status": status_key(status),
            "has_ui": false, "design": {"technical": null, "ux": null},
            "parent_id": null, "depends_on": []
        });
        serde_json::from_value(json).expect("ticket")
    }
    fn ty_key(t: TicketType) -> &'static str {
        match t {
            TicketType::Feature => "feature",
            TicketType::Bug => "bug",
            TicketType::Chore => "chore",
        }
    }

    #[test]
    fn counts_shipped_features_and_open_bugs() {
        let state = ProjectState {
            current_version: SemVer::new(1, 0, 0),
            tickets: vec![
                ticket("F001", TicketType::Feature, Status::Done),
                ticket("F002", TicketType::Feature, Status::InProgress),
                ticket("B001", TicketType::Bug, Status::Open),
                ticket("B002", TicketType::Bug, Status::Verified),
            ],
            history: vec![DeployRecord {
                version: SemVer::new(1, 0, 0),
                ticket: TicketId::new("F001").expect("id"),
                title: "t".to_owned(),
                at: "2026-07-12T10:00:00Z".to_owned(),
            }],
            ..ProjectState::default()
        };
        let m = compute(&state);
        assert_eq!(m.features_shipped, 1);
        assert_eq!(m.features_in_flight, 1);
        assert_eq!(m.bugs_open, 1);
        assert_eq!(m.bugs_verified, 1);
        assert_eq!(m.releases, 1);
        assert_eq!(m.feature_ratio_pct, 100);
        assert_eq!(m.deploys_by_day.len(), 1);
    }

    #[test]
    fn empty_state_is_all_zeroes() {
        let m = compute(&ProjectState::default());
        assert_eq!(m.total_tickets, 0);
        assert_eq!(m.releases, 0);
        assert_eq!(m.feature_ratio_pct, 0);
    }

    #[test]
    fn agent_evals_math() {
        let mut st = crate::state::ProjectState::default();
        st.spend.by_role.insert("dev_feature".into(), 2.0);
        st.spend
            .metered_cost_by_role
            .insert("dev_feature".into(), 2.0);
        st.spend.runs_by_role.insert("dev_feature".into(), 4);
        st.spend.total_cost_usd = 2.0;
        st.ticket_fail_attempts.insert("A-1".into(), 3);
        st.ticket_fail_attempts.insert("A-2".into(), 1);
        st.pr_fix_attempts.insert(7, 2);
        st.history.push(crate::state::DeployRecord {
            version: coxagent_domain::SemVer::new(1, 0, 0),
            ticket: coxagent_domain::TicketId::new("A-3").unwrap(),
            title: "t".into(),
            at: crate::state::now_rfc3339(),
        });
        let e = super::agent_evals(&st);
        assert_eq!(e.shipped_total, 1);
        assert_eq!(e.shipped_7d, 1);
        assert_eq!(e.parked, 1);
        assert_eq!(e.failed_attempts, 4);
        assert!((e.churn_per_ship - 4.0).abs() < 1e-9);
        assert_eq!(e.prs_stuck, 1);
        assert!((e.cost_per_ship_usd - 2.0).abs() < 1e-9);
        let dev = e.per_role.iter().find(|r| r.role == "dev_feature").unwrap();
        assert!((dev.avg_cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn days_back_handles_month_and_year_edges() {
        assert_eq!(super::days_back("2026-01-03", 7), "2025-12-27");
        assert_eq!(super::days_back("2026-03-02", 7), "2026-02-23");
    }

    #[test]
    fn tuning_hysteresis() {
        // Stalled (nothing shipped this week) → the intake brake's hysteresis is live.
        let mut e = super::AgentEvals {
            per_role: vec![],
            shipped_total: 10,
            shipped_7d: 0,
            parked: 0,
            failed_attempts: 20,
            churn_per_ship: 2.0,
            prs_stuck: 0,
            cost_per_ship_usd: 1.0,
        };
        let t0 = crate::state::Tuning::default();
        let t1 = super::decide_tuning(&e, 30, 0, &t0);
        assert!(t1.bugs_first, "hot churn trips the quality brake");
        assert!(t1.skip_ba, "fat backlog trips the intake brake");
        // Mid-band: nothing flips (hysteresis).
        e.churn_per_ship = 1.0;
        let t2 = super::decide_tuning(&e, 18, 0, &t1);
        assert!(t2.bugs_first && t2.skip_ba, "mid-band holds state");
        // Recovered: both release.
        e.churn_per_ship = 0.5;
        let t3 = super::decide_tuning(&e, 5, 0, &t2);
        assert!(!t3.bugs_first && !t3.skip_ba);
    }

    #[test]
    fn burndown_escalation_holds_brake_while_backlog_grows() {
        // CXA-F032: churn looks recovered, so the churn rule would release
        // features — but the open-bug backlog GREW today. The burn-down
        // escalation must hold the quality brake regardless.
        let e = super::AgentEvals {
            per_role: vec![],
            shipped_total: 10,
            shipped_7d: 3,
            parked: 0,
            failed_attempts: 0,
            churn_per_ship: 0.5,
            prs_stuck: 0,
            cost_per_ship_usd: 1.0,
        };
        let current = crate::state::Tuning {
            bugs_first: true,
            ..Default::default()
        };
        let grew = super::decide_tuning(&e, 12, -3, &current);
        assert!(
            grew.bugs_first,
            "a growing bug backlog holds the quality brake even with recovered churn"
        );
        // And once the backlog actually burns down, the release goes through.
        let burning = super::decide_tuning(&e, 12, 2, &current);
        assert!(
            !burning.bugs_first,
            "burning backlog + recovered churn resumes features"
        );
        // The escalation also TRIPS the brake from off when the backlog grows.
        let off = crate::state::Tuning::default();
        let tripped = super::decide_tuning(&e, 12, -1, &off);
        assert!(tripped.bugs_first, "growth escalates bugs_first from off");
    }

    #[test]
    fn intake_brake_releases_when_shipping_resumes() {
        // Production deadlock: a fat backlog parks the intake brake; without a
        // shipping-based recovery it stays parked forever because the parked BA
        // is what stops the backlog from draining below the release threshold.
        let mut e = super::AgentEvals {
            per_role: vec![],
            shipped_total: 3,
            shipped_7d: 0,
            parked: 0,
            failed_attempts: 0,
            churn_per_ship: 0.4,
            prs_stuck: 0,
            cost_per_ship_usd: 1.0,
        };
        let t0 = crate::state::Tuning::default();
        // Huge backlog + nothing shipping → brake parks (defensive overload stop).
        let parked = super::decide_tuning(&e, 84, 0, &t0);
        assert!(parked.skip_ba, "stalled overload parks the intake brake");
        // Team ships → the brake must release even while the 84-ticket backlog remains.
        e.shipped_7d = 2;
        let recovered = super::decide_tuning(&e, 84, 0, &parked);
        assert!(
            !recovered.skip_ba,
            "shipping must un-park BA at a fat backlog"
        );
    }

    #[test]
    fn intake_brake_stays_off_while_shipping() {
        // When the team is shipping, a big backlog must NOT hard-park BA — that
        // starves the very work that drains the queue.
        let e = super::AgentEvals {
            per_role: vec![],
            shipped_total: 3,
            shipped_7d: 3,
            parked: 0,
            failed_attempts: 0,
            churn_per_ship: 0.4,
            prs_stuck: 0,
            cost_per_ship_usd: 1.0,
        };
        let out = super::decide_tuning(&e, 84, 0, &crate::state::Tuning::default());
        assert!(
            !out.skip_ba,
            "shipping team must keep proposing at a fat backlog"
        );
    }
}
