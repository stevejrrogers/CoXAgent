// Part of the cycle module split by concern — see cycle/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Periodic health reviews the cycle triggers: architecture, docs, and the one-spec realignment pass.

use super::*;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Whole-system architecture review: the SA examines the codebase against
    /// clean architecture / DDD / SOLID, coupling and module boundaries,
    /// monolith-vs-microservices fit, and horizontal scalability — then files
    /// concrete refactor chores and asks the PO to prioritise them.
    pub(super) async fn architecture_audit(&self, sprint: u32) {
        self.report("SA", "architecture review");
        let uc = crate::use_cases::RunArchitectureAuditUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
            self.config.workflow.token_saver,
            self.config.workflow.language,
        )
        .with_files(self.files.clone());
        if let Err(e) = uc.execute(sprint).await {
            tracing::warn!("architecture review: {e}");
        }
    }

    /// Documentation review (same cadence as the architecture review): DOCS scans
    /// the Wiki for shipped work that has no page (or only a thin stub) and writes
    /// the missing documentation in full — so the knowledge base doesn't drift
    /// behind the code.
    pub(super) async fn docs_audit(&self, sprint: u32) {
        self.report("DOCS", "documentation review");
        let uc = crate::use_cases::RunDocsAuditUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
            self.config.workflow.language,
        );
        if let Err(e) = uc.execute(sprint).await {
            tracing::warn!("documentation review: {e}");
        }
    }

    /// While refactoring, the SA rewrites one pending feature's technical design
    /// so it targets the new architecture instead of the old bad one (bounded to
    /// one per cycle; marked so it isn't redone).
    pub(super) async fn realign_one_spec(&self, vi: bool) {
        use coxagent_domain::{Status, TicketType};
        let Ok(state) = self.store.load().await else {
            return;
        };
        let target = state
            .tickets
            .iter()
            .filter(|t| {
                t.ticket_type() == TicketType::Feature
                    && matches!(t.status(), Status::Pending | Status::Ready)
            })
            .find_map(|t| {
                let d = t.design().technical.as_ref()?;
                (!d.approach.contains("[realigned]"))
                    .then(|| (t.id().clone(), t.title().to_owned(), d.approach.clone()))
            });
        let Some((id, title, approach)) = target else {
            return;
        };
        self.report("SA", "realigning spec");
        let task = format!(
            "The team is in a REFACTOR SPRINT fixing the architecture. Update the technical design \
             for feature {id} ({title}) so it targets the NEW clean architecture, not the old one \
             it was written against. Current approach:\n{approach}\n\nRespond with ONLY JSON: \
             {{\"approach\": string, \"files\": [string], \"api_contract\": string, \
             \"data_changes\": string, \"test_plan\": string}}. Begin `approach` with \
             \"[realigned] \"."
        );
        let request = AgentRequest {
            role: coxagent_domain::Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(600),
            escalation_level: 0,
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return;
        };
        if !outcome.succeeded() {
            return;
        }
        let (Some(start), Some(end)) = (outcome.stdout.find('{'), outcome.stdout.rfind('}')) else {
            return;
        };
        let Ok(design) =
            serde_json::from_str::<coxagent_domain::TechnicalDesign>(&outcome.stdout[start..=end])
        else {
            return;
        };
        if let Ok(mut s) = self.store.load().await {
            if let Some(t) = s.ticket_mut(&id) {
                if t.set_technical_design(coxagent_domain::Role::Sa, design)
                    .is_ok()
                {
                    let msg = if vi {
                        format!("🧭 SA đã cập nhật lại technical spec cho {id} theo kiến trúc mới.")
                    } else {
                        format!(
                            "🧭 SA realigned the technical spec for {id} to the new architecture."
                        )
                    };
                    s.post_comment("SA", &msg, Some(id.to_string()));
                    let _ = self.store.save(&s).await;
                }
            }
        }
    }
}

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// The adaptive gate: let routine designed tickets through with an undo
    /// window, ask about the rest, and learn which is which from what humans
    /// decided (docs/ADAPTIVE_APPROVAL.md).
    pub(super) async fn adaptive_approval_pass(&self) {
        let cfg = &self.config.workflow.human;
        if !cfg.gate_ready || !cfg.adaptive.enabled {
            return;
        }
        let cap = cfg.adaptive.max_auto_per_cycle();
        let learn_after = cfg.adaptive.learn_after_samples();
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            use crate::use_cases::approval_memory::{announce, rule_for, Rule};
            use crate::use_cases::approval_risk::{assess, shape_key, Lane};
            use coxagent_domain::Status;

            // Prior art per shape: what already reached a good terminal state.
            let mut shipped: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for t in &s.tickets {
                if matches!(
                    t.status(),
                    Status::Done | Status::Documented | Status::Verified
                ) {
                    *shipped.entry(shape_key(t)).or_insert(0) += 1;
                }
            }
            let samples = s.approval_samples.clone();
            let asked_again = s.ask_again_shapes.clone();
            let parked: std::collections::BTreeSet<String> =
                s.ticket_fail_attempts.keys().cloned().collect();

            // Candidates: designed, waiting behind the ready gate.
            let candidates: Vec<coxagent_domain::TicketId> = s
                .tickets
                .iter()
                .filter(|t| t.status() == Status::Pending && t.design().technical.is_some())
                .map(|t| t.id().clone())
                .collect();

            let mut announced: Vec<String> = Vec::new();
            let mut promoted = 0usize;
            for id in candidates {
                if promoted >= cap {
                    break;
                }
                let Some(ticket) = s.ticket(&id) else { continue };
                let shape = shape_key(ticket);
                if asked_again.contains(&shape) {
                    continue; // a human overrode this shape: always ask
                }
                let verdict = assess(
                    ticket,
                    shipped.get(&shape).copied().unwrap_or(0),
                    parked.contains(&id.to_string()),
                );
                let learned = rule_for(&shape, &samples, learn_after);
                let allow = match (&verdict.lane, &learned) {
                    // Risk says routine — proceed unless a human reversed one.
                    (Lane::Auto, Rule::KeepAsking | Rule::AutoApprove { .. }) => {
                        !matches!(learned, Rule::PreflightFix { .. })
                    }
                    // Risk says ask, but this team approves the shape every
                    // time — trust the humans over the heuristic.
                    (Lane::Ask, Rule::AutoApprove { .. }) => true,
                    _ => false,
                };
                if !allow {
                    continue;
                }
                let title = ticket.title().to_owned();
                let Some(t) = s.ticket_mut(&id) else { continue };
                if t.transition_to(coxagent_domain::Role::System, Status::Ready)
                    .is_err()
                {
                    continue;
                }
                s.auto_approved_at
                    .insert(id.to_string(), crate::state::now_rfc3339());
                announced.push(format!(
                    "🤖 {id} auto-approved — {} ({}). {title}. Undo within {} minutes if this \
                     needed a person.",
                    verdict.why,
                    format_args!("risk {}", verdict.score),
                    cfg_undo_minutes()
                ));
                promoted += 1;
                if let Some(msg) = announce(&shape, &learned) {
                    if !s.decisions.contains(&msg) {
                        s.decisions.push(msg.clone());
                        announced.push(msg);
                    }
                }
            }
            for msg in announced {
                s.post_chat_in("SYSTEM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            }
            Ok(())
        })
        .await;
    }
}

/// The undo window in minutes, for the announcement text. Kept as a free
/// function so the closure above does not have to capture the whole config.
fn cfg_undo_minutes() -> u64 {
    30
}
