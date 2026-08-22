// Part of the cycle module split by concern — see cycle/mod.rs.
//! The trend sentinel: the team's STRATEGIC eye.
//!
//! Every role reacts to its own queue; nobody was reading the week. Bug inflow
//! outrunning outflow, one module spawning half the failures, D-grade cycles
//! clustering, spend rising while shipping falls — all visible in state, all
//! unread. Once a week the sentinel aggregates those numbers DETERMINISTICALLY
//! (zero tokens), hands the digest to ONE engine call in the PO seat, and
//! turns the answer into: a Wiki trend report + up to three `[trend]` tickets
//! filed as Pending — which face the same refinement/approval gates as any
//! other ticket, so strategy is proposed by the machine and DECIDED by people.

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use coxagent_domain::{Status, TicketType};

/// The engine's JSON answer: a short report and concrete strategic actions.
#[derive(Debug, serde::Deserialize)]
struct SentinelOutput {
    #[serde(default)]
    report_md: String,
    #[serde(default)]
    actions: Vec<SentinelAction>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SentinelAction {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    /// `"chore"` (default) or `"feature"` — the sentinel proposes work about
    /// the SYSTEM (pay down module X, add missing gate), not product features.
    #[serde(default)]
    kind: String,
}

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// One weekly strategic pass. Leader-only; date-gated via `daily_jobs`
    /// ("trend_sentinel" holds the last run date). Best-effort: any failure
    /// skips the week, never the cycle.
    pub(super) async fn trend_sentinel(&self) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        let today = crate::state::now_rfc3339()[..10].to_owned();
        if let Some(last) = state.daily_jobs.get("trend_sentinel") {
            if super::release_cut::days_between(last, &today) < 7 {
                return;
            }
        }
        let digest = build_digest(&state);
        // Not enough history to say anything a human would respect.
        if state.cycle_scores.len() < 10 {
            return;
        }
        let request = AgentRequest {
            role: coxagent_domain::Role::Po,
            system_prompt: crate::prompts::system_prompt(crate::prompts::PO),
            task_prompt: format!(
                "You are reviewing your team's WEEK, not a ticket. Below is the raw \
                 operational digest. Find the 2-3 trends that MATTER (bug inflow vs \
                 outflow, where failures cluster, grade/cost direction, carried-over \
                 work), say what they mean, and propose AT MOST 3 concrete strategic \
                 actions about the SYSTEM (e.g. 'harden module X before new features', \
                 'add a gate for Y', 'split Z'). No generic advice — every claim must \
                 cite a number from the digest.\n\n{digest}\n\n\
                 Respond with ONLY JSON: {{\"report_md\":\"## Trend report\\n…(markdown, \
                 <=300 words)\",\"actions\":[{{\"title\":\"…\",\"description\":\"what + why + \
                 acceptance criteria\",\"kind\":\"chore\"|\"feature\"}}]}}"
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(600),
            escalation_level: 0,
            label: Some("trend-sentinel".to_owned()),
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return;
        };
        if !outcome.succeeded() {
            return;
        }
        let Some(parsed) = parse_sentinel(&outcome.stdout) else {
            return;
        };
        let report = parsed.report_md.trim().to_owned();
        if report.is_empty() {
            return;
        }
        let actions: Vec<SentinelAction> = parsed
            .actions
            .into_iter()
            .filter(|a| !a.title.trim().is_empty())
            .take(3)
            .collect();
        let n_actions = actions.len();
        self.apply_sentinel(&today, &report, &actions, n_actions).await;
    }

    /// Persist the sentinel's answer: report page, `[trend]` tickets, chat.
    async fn apply_sentinel(
        &self,
        today: &str,
        report: &str,
        actions: &[SentinelAction],
        n_actions: usize,
    ) {
        let today = today.to_owned();
        let report = report.to_owned();
        let actions = actions.to_vec();
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            s.daily_jobs
                .insert("trend_sentinel".to_owned(), today.clone());
            // The report is a Wiki page under Operations, one per week.
            let title = format!("Trend report {today}");
            s.upsert_doc("", "Operations", "ops", &title, &report, "PO");
            // Each action becomes a PENDING ticket — the normal refinement
            // and approval gates decide its fate; the sentinel only proposes.
            for a in &actions {
                let title = format!("[trend] {}", a.title.trim());
                let dup = s
                    .tickets
                    .iter()
                    .any(|t| t.title() == title && t.status() != Status::Rejected);
                if dup {
                    continue;
                }
                let kind = if a.kind == "feature" {
                    TicketType::Feature
                } else {
                    TicketType::Chore
                };
                let Ok(id) = crate::use_cases::add_ticket::mint_id(kind, s) else {
                    continue;
                };
                if let Ok(t) = coxagent_domain::Ticket::new(
                    id,
                    kind,
                    &title,
                    a.description.trim(),
                    coxagent_domain::Priority::Medium,
                    coxagent_domain::Complexity::Medium,
                    false,
                ) {
                    s.tickets.push(t);
                }
            }
            s.log_activity("PO", "weekly trend report", None);
            s.post_chat_in(
                "PO",
                &format!(
                    "📈 Weekly trend report is up (Wiki → Operations → Trend report \
                     {today}) — {n_actions} strategic ticket(s) proposed for refinement."
                ),
                crate::state::AGENTS_CHANNEL,
                Vec::new(),
            );
            Ok(())
        })
        .await;
        self.notify(
            "trend_report",
            format!("weekly trend report published ({n_actions} strategic proposals)"),
        )
        .await;
    }
}

/// First `{`..last `}` JSON, tolerant of engine prose around it.
fn parse_sentinel(raw: &str) -> Option<SentinelOutput> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    serde_json::from_str(raw.get(start..=end)?).ok()
}

/// The week in numbers, computed from state alone — zero tokens. Every line
/// is something the PO prompt can cite; nothing here guesses.
fn build_digest(state: &crate::state::ProjectState) -> String {
    use std::fmt::Write as _;
    let mut d = String::from("## Operational digest\n");
    // Scorecard window: last 7 days of recorded cycles.
    let week: Vec<&crate::state::CycleScore> = state
        .cycle_scores
        .iter()
        .filter(|c| {
            crate::use_cases::cycle::seconds_since_public(&c.at)
                .is_some_and(|s| s <= 7 * 86_400)
        })
        .collect();
    let (mut a, mut b, mut cg, mut dg, mut cost, mut shipped, mut runs) =
        (0u64, 0u64, 0u64, 0u64, 0.0f64, 0u64, 0u64);
    for c in &week {
        match c.grade.as_str() {
            "A" => a += 1,
            "B" => b += 1,
            "C" => cg += 1,
            _ => dg += 1,
        }
        cost += c.cost_usd;
        shipped += c.shipped;
        runs += c.runs;
    }
    let _ = writeln!(
        d,
        "cycles_7d: {} (A:{a} B:{b} C:{cg} D:{dg}) | engine_runs: {runs} | \
         spend_usd: {cost:.2} | shipped: {shipped} | usd_per_ship: {}",
        week.len(),
        if shipped > 0 {
            {
                #[allow(clippy::cast_precision_loss)]
                let per = cost / shipped as f64;
                format!("{per:.2}")
            }
        } else {
            "n/a (nothing shipped)".to_owned()
        }
    );
    // Bugs: stock, and this week's outflow (merges stamped on B-tickets).
    let open_bugs = state
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
        .count();
    let fixed_7d = state
        .ticket_last_merge
        .iter()
        .filter(|(k, at)| {
            k.contains("-B")
                && crate::use_cases::cycle::seconds_since_public(at)
                    .is_some_and(|s| s <= 7 * 86_400)
        })
        .count();
    let _ = writeln!(d, "bugs: open_now {open_bugs} | merged_fixes_7d {fixed_7d}");
    // Where failures cluster: files named in structured attempt failures.
    let mut file_hits: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    for f in state.ticket_failures.values().flatten() {
        for file in &f.files {
            *file_hits.entry(file.as_str()).or_default() += 1;
        }
    }
    let mut hot: Vec<(&str, u32)> = file_hits.into_iter().collect();
    hot.sort_by_key(|x| std::cmp::Reverse(x.1));
    if !hot.is_empty() {
        let top: Vec<String> = hot
            .iter()
            .take(5)
            .map(|(f, n)| format!("{f} ({n})"))
            .collect();
        let _ = writeln!(d, "failure_hotspots: {}", top.join(", "));
    }
    // Sprint honesty: last 5 sprints committed vs done.
    let last: Vec<String> = state
        .sprints
        .iter()
        .rev()
        .take(5)
        .map(|r| format!("#{} {}/{}", r.number, r.done, r.committed))
        .collect();
    if !last.is_empty() {
        let _ = writeln!(d, "sprints_done/committed (newest first): {}", last.join(", "));
    }
    // Spend by role this lifetime window (metered).
    let mut roles: Vec<(&String, &f64)> = state.spend.metered_cost_by_role.iter().collect();
    roles.sort_by(|x, y| y.1.partial_cmp(x.1).unwrap_or(std::cmp::Ordering::Equal));
    let top_roles: Vec<String> = roles
        .iter()
        .take(4)
        .map(|(r, c)| format!("{r} ${c:.2}"))
        .collect();
    if !top_roles.is_empty() {
        let _ = writeln!(d, "cost_by_role_total: {}", top_roles.join(", "));
    }
    let _ = writeln!(
        d,
        "open_prs: {} | human_holds: {} | engine_incidents_open: {}",
        state.open_prs.len(),
        state.human_holds.len(),
        state.engine_incidents.len()
    );
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_reports_the_week_in_numbers() {
        let mut s = crate::state::ProjectState::default();
        s.cycle_scores.push(crate::state::CycleScore {
            cycle: 1,
            at: crate::state::now_rfc3339(),
            runs: 5,
            useful: 3,
            cost_usd: 12.5,
            shipped: 2,
            incidents: 0,
            errors: 0,
            grade: "A".to_owned(),
            ..crate::state::CycleScore::default()
        });
        let d = build_digest(&s);
        assert!(d.contains("A:1"), "{d}");
        assert!(d.contains("spend_usd: 12.50"), "{d}");
        assert!(d.contains("usd_per_ship: 6.25"), "{d}");
        assert!(d.contains("open_now 0"), "{d}");
    }

    #[test]
    fn sentinel_json_parses_with_prose_around_it() {
        let raw = "here you go\n{\"report_md\":\"## Trend report\",\"actions\":[{\"title\":\"t\",\"description\":\"d\",\"kind\":\"chore\"}]}\nthanks";
        let out = parse_sentinel(raw).expect("parse");
        assert_eq!(out.actions.len(), 1);
        assert!(out.report_md.starts_with("## Trend"));
    }
}
