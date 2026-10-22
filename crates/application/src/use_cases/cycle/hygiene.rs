// One logical module split across files for merge-conflict surface — see
// use_cases/cycle/mod.rs.
//! Backlog hygiene pass — the SM proactively triaging the on-hold mountain
//! instead of Claude (or Steve) doing it by hand.
//!
//! Two layers, cheapest first:
//! 1. MECHANICAL floor (`crate::backlog_hygiene::plan`, pure + tested):
//!    resume childless non-bug holds, reject exact-title duplicates.
//! 2. SM JUDGMENT on what the floor only reports: the held bugs whose rescue
//!    budget ran out and other ambiguous parks go to the SM engine with their
//!    failure history, and it decides resume / reject / keep WITH A REASON —
//!    the reasons land in team chat as one digest, so every triage is
//!    auditable and reversible instead of silent.
//!
//! Once per day, leader only (`daily_jobs` key `backlog_hygiene`). Action
//! caps keep one pass from stampeding the queue.

use super::*;
use crate::ProjectState;

/// What the SM may do to one parked ticket, with its reason.
#[derive(Debug, serde::Deserialize)]
struct TriageDecision {
    id: String,
    /// `resume` | `reject` | `keep`
    action: String,
    reason: String,
}

const MAX_AUTO_ACTIONS: usize = 30;
const MAX_JUDGMENT_ITEMS: usize = 15;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Daily backlog-hygiene pass. See the module docs.
    #[allow(clippy::too_many_lines)] // one pass, read top to bottom (house style)
    pub(super) async fn backlog_hygiene_pass(&self) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        let today = crate::state::now_rfc3339()[..10].to_owned();
        if state.daily_jobs.get("backlog_hygiene") == Some(&today) {
            return;
        }
        // Mechanical floor over one snapshot.
        let live_parents: std::collections::HashSet<String> = state
            .tickets
            .iter()
            .filter(|t| {
                !matches!(
                    t.status(),
                    coxagent_domain::Status::Documented
                        | coxagent_domain::Status::Verified
                        | coxagent_domain::Status::Rejected
                        | coxagent_domain::Status::Done
                )
            })
            .filter_map(|t| t.parent_id().map(ToString::to_string))
            .collect();
        let views: Vec<crate::backlog_hygiene::HygieneTicket> = state
            .tickets
            .iter()
            .map(|t| crate::backlog_hygiene::HygieneTicket {
                id: t.id().to_string(),
                title: t.title().to_owned(),
                status: status_key(t.status()),
                is_bug: t.ticket_type() == coxagent_domain::ticket::TicketType::Bug,
                has_live_children: live_parents.contains(&t.id().to_string()),
                created_at: t.created_at().unwrap_or_default().to_owned(),
            })
            .collect();
        let hplan = crate::backlog_hygiene::plan(&views);
        self.report("SM", "backlog hygiene (daily triage)");
        let resumes: Vec<String> = hplan
            .resume
            .iter()
            .take(MAX_AUTO_ACTIONS)
            .cloned()
            .collect();
        let rejects: Vec<(String, String)> = hplan
            .reject_duplicates
            .iter()
            .take(MAX_AUTO_ACTIONS)
            .cloned()
            .collect();
        let (mut n_resumed, mut n_rejected) = (0usize, 0usize);
        let r2 = resumes.clone();
        let j2 = rejects.clone();
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            s.daily_jobs
                .insert("backlog_hygiene".to_owned(), today.clone());
            for id in &r2 {
                if let Some(t) = s.tickets.iter_mut().find(|t| t.id().as_str() == id) {
                    let _ = t.transition_to(
                        coxagent_domain::Role::Sm,
                        coxagent_domain::Status::Pending,
                    );
                }
            }
            for (dupe, keep) in &j2 {
                if let Some(t) = s.tickets.iter_mut().find(|t| t.id().as_str() == dupe) {
                    if t.transition_to(
                        coxagent_domain::Role::System,
                        coxagent_domain::Status::Rejected,
                    )
                    .is_ok()
                    {
                        s.post_comment(
                            "SM",
                            &format!("♻️ rejected as an exact-title duplicate of {keep} (backlog hygiene)"),
                            Some(dupe.clone()),
                        );
                    }
                }
            }
            Ok(())
        })
        .await;
        // Re-count from the actual transitions, not the plan.
        if let Ok(after) = self.store.load().await {
            for id in &resumes {
                if after.tickets.iter().any(|t| {
                    t.id().as_str() == id && t.status() == coxagent_domain::Status::Pending
                }) {
                    n_resumed += 1;
                }
            }
            for (dupe, _) in &rejects {
                if after.tickets.iter().any(|t| {
                    t.id().as_str() == dupe && t.status() == coxagent_domain::Status::Rejected
                }) {
                    n_rejected += 1;
                }
            }
        }
        // SM judgment layer over the reported pile.
        let judged = self.sm_judgment(&state, &hplan.report_held).await;
        let digest = format!(
            "🧹 Backlog hygiene: resumed {n_resumed} parked ticket(s), rejected {n_rejected} \
             duplicate(s), {} long-held item(s) triaged by SM judgment, {} still parked \
             (split parents or awaiting humans).",
            judged,
            hplan.report_held.len().saturating_sub(judged)
        );
        tracing::info!("{digest}");
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            s.post_chat_in("SM", &digest, crate::state::AGENTS_CHANNEL, Vec::new());
            Ok(())
        })
        .await;
    }

    /// Ask the SM engine to triage up to [`MAX_JUDGMENT_ITEMS`] long-held
    /// tickets, each with its failure history; apply only legal transitions
    /// and post every decision's reason. Returns how many decisions applied.
    #[allow(clippy::too_many_lines)] // one judgment loop, read top to bottom
    async fn sm_judgment(&self, state: &ProjectState, held: &[String]) -> usize {
        let items: Vec<String> = held
            .iter()
            .take(MAX_JUDGMENT_ITEMS)
            .filter_map(|id| {
                let t = state.ticket(&coxagent_domain::TicketId::new(id.clone()).ok()?)?;
                let fails = state
                    .ticket_failures
                    .get(id)
                    .map(|f| {
                        f.iter()
                            .rev()
                            .take(2)
                            .map(|a| a.detail.chars().take(160).collect::<String>())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    })
                    .unwrap_or_default();
                let attempts = state.ticket_fail_attempts.get(id).copied().unwrap_or(0);
                Some(format!(
                    "- {id} [{}] \"{}\" · {attempts} failed attempt(s) · last failures: {}",
                    status_key(t.status()),
                    t.title(),
                    if fails.is_empty() {
                        "none recorded"
                    } else {
                        &fails
                    }
                ))
            })
            .collect();
        if items.is_empty() {
            return 0;
        }
        let task = format!(
            "You are the Scrum Master triaging LONG-PARKED tickets. For each, decide like a \
             seasoned lead pushing the project forward: resume it (it still matters and can \
             succeed now that main is green), reject it (obsolete, superseded, or repeatedly \
             failing with no path), or keep it parked (genuinely blocked on a human). Be \
             decisive — a parked ticket helps nobody.\n\nTickets:\n{}\n\nRespond with ONLY a \
             JSON array: [{{\"id\": string, \"action\": \"resume\"|\"reject\"|\"keep\", \
             \"reason\": string (one sentence)}}] covering every listed id.",
            items.join("\n")
        );
        let request = AgentRequest {
            role: coxagent_domain::Role::Sm,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SM),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(600),
            escalation_level: 0,
            label: Some("backlog-triage".to_owned()),
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return 0;
        };
        if !outcome.succeeded() {
            return 0;
        }
        let (Some(a), Some(b)) = (outcome.stdout.find('['), outcome.stdout.rfind(']')) else {
            return 0;
        };
        let Ok(decisions) = serde_json::from_str::<Vec<TriageDecision>>(&outcome.stdout[a..=b])
        else {
            return 0;
        };
        let allowed: std::collections::HashSet<&String> = held.iter().collect();
        let mut applied = 0usize;
        for d in decisions {
            if !allowed.contains(&d.id) {
                continue; // the model may not invent targets
            }
            let (id, action, reason) = (d.id, d.action, d.reason);
            let ok = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                let Some(t) = s.tickets.iter_mut().find(|t| t.id().as_str() == id) else {
                    return Ok(());
                };
                let note = match action.as_str() {
                    "resume" => {
                        // Bugs resume to Open (their lifecycle has no Pending).
                        let target = if t.ticket_type() == coxagent_domain::ticket::TicketType::Bug
                        {
                            coxagent_domain::Status::Open
                        } else {
                            coxagent_domain::Status::Pending
                        };
                        t.transition_to(coxagent_domain::Role::Sm, target)
                            .is_ok()
                            .then(|| format!("▶️ SM resumed from hold: {reason}"))
                    }
                    "reject" => t
                        .transition_to(
                            coxagent_domain::Role::System,
                            coxagent_domain::Status::Rejected,
                        )
                        .is_ok()
                        .then(|| format!("🗑️ SM rejected after triage: {reason}")),
                    _ => Some(format!("⏸️ SM keeps parked: {reason}")),
                };
                if let Some(msg) = note {
                    s.post_comment("SM", &msg, Some(id.clone()));
                }
                Ok(())
            })
            .await
            .is_ok();
            if ok {
                applied += 1;
            }
        }
        applied
    }
}

/// The serde-style key for a status (`on_hold`, not `OnHold`).
fn status_key(s: coxagent_domain::Status) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}
