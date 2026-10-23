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

#[derive(serde::Deserialize)]
struct GateIdea {
    title: String,
    description: String,
}

const MAX_AUTO_ACTIONS: usize = 30;
const MAX_JUDGMENT_ITEMS: usize = 15;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Daily backlog-hygiene pass. See the module docs.
    /// Bounded wrapper: whatever goes wrong inside (a stuck engine, a store
    /// that never answers), the CYCLE must keep breathing — the 2026-10-22
    /// boot wedged exactly here on the pass's first prod run, silently.
    pub(super) async fn backlog_hygiene_pass(&self) {
        if tokio::time::timeout(
            std::time::Duration::from_secs(900),
            self.backlog_hygiene_inner(),
        )
        .await
        .is_err()
        {
            tracing::warn!(
                "backlog hygiene: pass exceeded its 15-minute budget — aborted; the \
                 cycle continues and the pass retries at its next daily slot"
            );
        }
    }

    #[allow(clippy::too_many_lines)] // one pass, read top to bottom (house style)
    async fn backlog_hygiene_inner(&self) {
        tracing::info!("backlog hygiene: loading state");
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
        tracing::info!(
            "backlog hygiene: plan — {} resume, {} duplicate-reject, {} for SM judgment",
            hplan.resume.len(),
            hplan.reject_duplicates.len(),
            hplan.report_held.len()
        );
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
        tracing::info!("backlog hygiene: mechanical actions applied — starting SM judgment");
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

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Lesson→gate pass (CXA-F431): once a day, the SA looks at the week's
    /// recurring failure clusters and lessons and proposes PERMANENT automated
    /// gates — the move that turned the copyText shadowing regressions into
    /// the web-bundle gate, done by the team instead of an outside reviewer.
    /// At most two gate tickets a day, deduped by title against the backlog.
    #[allow(clippy::too_many_lines)] // one pass, read top to bottom (house style)
    pub(super) async fn lesson_to_gate_pass(&self) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        let today = crate::state::now_rfc3339()[..10].to_owned();
        if state.daily_jobs.get("lesson_to_gate") == Some(&today) {
            return;
        }
        // Evidence: failure details that occurred on 2+ distinct tickets
        // (same leading words), plus the newest recorded lessons.
        let mut clusters: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (tid, fails) in &state.ticket_failures {
            for f in fails.iter().rev().take(1) {
                let key: String = f
                    .detail
                    .split_whitespace()
                    .take(8)
                    .collect::<Vec<_>>()
                    .join(" ");
                if key.len() >= 20 {
                    clusters.entry(key).or_default().push(tid.clone());
                }
            }
        }
        let recurring: Vec<String> = clusters
            .into_iter()
            .filter(|(_, tids)| tids.len() >= 2)
            .map(|(k, tids)| {
                format!(
                    "- \"{k}…\" hit {} tickets ({})",
                    tids.len(),
                    tids.join(", ")
                )
            })
            .take(10)
            .collect();
        let lessons: Vec<String> = state.lessons.iter().rev().take(10).cloned().collect();
        if recurring.is_empty() && lessons.is_empty() {
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                s.daily_jobs
                    .insert("lesson_to_gate".to_owned(), today.clone());
                Ok(())
            })
            .await;
            return;
        }
        self.report("SA", "lesson→gate (daily)");
        let task = format!(
            "You are the Solution Architect. Below are this project's RECURRING failure \
             clusters and recent lessons. Propose at most TWO permanent automated GATES \
             (a test, CI step, or check) that would make a recurrence IMPOSSIBLE instead \
             of merely remembered — e.g. 'every web/js module must be registered in the \
             bundle' became a one-test gate. Only propose a gate when the failure class \
             is mechanical enough to check automatically; otherwise propose nothing.\n\n\
             Recurring failures:\n{}\n\nRecent lessons:\n{}\n\nRespond with ONLY a JSON \
             array (possibly empty): [{{\"title\": \"gate: …\", \"description\": \"what \
             the gate checks + where it lives + acceptance criteria\"}}]",
            if recurring.is_empty() {
                "(none)".to_owned()
            } else {
                recurring.join("\n")
            },
            if lessons.is_empty() {
                "(none)".to_owned()
            } else {
                lessons.join("\n")
            }
        );
        let request = AgentRequest {
            role: coxagent_domain::Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(600),
            escalation_level: 0,
            label: Some("lesson-to-gate".to_owned()),
        };
        let proposals: Vec<(String, String)> = match self.engine.run(request).await {
            Ok(o) if o.succeeded() => {
                let (Some(a), Some(b)) = (o.stdout.find('['), o.stdout.rfind(']')) else {
                    return;
                };
                serde_json::from_str::<Vec<GateIdea>>(&o.stdout[a..=b])
                    .map(|v| {
                        v.into_iter()
                            .take(2)
                            .map(|g| (g.title, g.description))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            _ => return,
        };
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            s.daily_jobs.insert("lesson_to_gate".to_owned(), today.clone());
            let mut filed = 0usize;
            for (title, description) in &proposals {
                let t = title.trim();
                if t.len() < 12
                    || s.tickets
                        .iter()
                        .any(|x| x.title().eq_ignore_ascii_case(t))
                {
                    continue;
                }
                // Deterministic id from the title (sweep pattern): refiling
                // the same proposal tomorrow dedupes by id as well as title.
                let mut h: u32 = 2_166_136_261;
                for b in t.bytes() {
                    h ^= u32::from(b);
                    h = h.wrapping_mul(16_777_619);
                }
                let Ok(id) =
                    coxagent_domain::TicketId::new(format!("GATE-{:06X}", h & 0xFF_FFFF))
                else {
                    continue;
                };
                if s.tickets.iter().any(|x| x.id() == &id) {
                    continue;
                }
                if let Ok(ticket) = coxagent_domain::Ticket::new(
                    id,
                    coxagent_domain::ticket::TicketType::Chore,
                    t.to_owned(),
                    format!("{description}\n\n(filed by the daily lesson→gate pass, CXA-F431)"),
                    coxagent_domain::Priority::Medium,
                    coxagent_domain::ticket::Complexity::Medium,
                    false,
                ) {
                    s.tickets.push(ticket);
                    filed += 1;
                }
            }
            if filed > 0 {
                s.post_chat_in(
                    "SA",
                    &format!("🚧 lesson→gate: filed {filed} gate ticket(s) from recurring failure clusters."),
                    crate::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
            }
            Ok(())
        })
        .await;
    }
}
