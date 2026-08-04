//! Fill the gap BEFORE a human sees the ticket.
//!
//! docs/ADAPTIVE_APPROVAL.md promises that a rejection reason becomes a
//! pre-flight check — "no acceptance criteria" should mean the BA fills them
//! in, not that the same shape reaches the inbox again. Nothing implemented
//! that half: `Rule::PreflightFix` only *blocked* the auto lane, so AC-less
//! tickets piled up waiting for a person who could only reject them for the
//! reason the machine already knew.
//!
//! A ticket with no acceptance criteria also scores +25 risk, which alone puts
//! it over the auto lane's threshold. So this pass is what lets routine work
//! actually flow: give it a testable definition of done and it can be judged.

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use coxagent_domain::Status;

/// At most this many tickets get an engine call per cycle — the pass is a
/// convenience, not a reason for the cycle to become an AC-writing service.
const MAX_PER_CYCLE: usize = 3;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Ask the BA to write acceptance criteria for designed tickets that have
    /// none, so the human gate judges a complete ticket or the adaptive gate
    /// can let a routine one through.
    pub(super) async fn preflight_acceptance_criteria(&self) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        let targets: Vec<(String, String, String)> = state
            .tickets
            .iter()
            .filter(|t| {
                t.status() == Status::Pending
                    && t.design().technical.is_some()
                    && t.acceptance_criteria().is_empty()
            })
            .take(MAX_PER_CYCLE)
            .map(|t| {
                (
                    t.id().to_string(),
                    t.title().to_owned(),
                    t.description().to_owned(),
                )
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        self.report("BA", "writing acceptance criteria");
        for (id, title, description) in targets {
            let Some(criteria) = self.ask_for_criteria(&id, &title, &description).await else {
                continue;
            };
            let Ok(mut s) = self.store.load().await else {
                return;
            };
            let Some(t) = s.tickets.iter_mut().find(|t| t.id().as_str() == id) else {
                continue;
            };
            // Between the read and now a person may have written them.
            if !t.acceptance_criteria().is_empty() {
                continue;
            }
            t.set_acceptance_criteria(criteria.clone());
            s.post_comment(
                "BA",
                &format!(
                    "📋 Acceptance criteria added before this reached anyone's inbox:\n- {}",
                    criteria.join("\n- ")
                ),
                Some(id.clone()),
            );
            let _ = self.store.save(&s).await;
        }
    }

    /// One engine call: the criteria for a ticket, or `None` when the answer
    /// was not usable. Never invents a whole requirement — the ticket's own
    /// title and description are the source.
    async fn ask_for_criteria(
        &self,
        id: &str,
        title: &str,
        description: &str,
    ) -> Option<Vec<String>> {
        let request = AgentRequest {
            role: coxagent_domain::Role::Ba,
            system_prompt: crate::prompts::system_prompt(crate::prompts::BA),
            task_prompt: format!(
                "Ticket {id} ('{title}') has no acceptance criteria, so nobody can tell when it \
                 is done. Write 3-5 criteria that are OBSERVABLE and CHECKABLE — each one a \
                 thing someone could verify by running or reading something, not a restatement \
                 of the title. Use only what the ticket already says; invent no new scope.\
                 \n\nDescription:\n{description}\n\nRespond with ONLY a JSON array of strings."
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(300),
            escalation_level: 0,
        };
        let out = self.engine.run(request).await.ok()?;
        if !out.succeeded() {
            return None;
        }
        let raw = &out.stdout;
        let (start, end) = (raw.find('[')?, raw.rfind(']')?);
        if end < start {
            return None;
        }
        let items: Vec<String> = serde_json::from_str(&raw[start..=end]).ok()?;
        let criteria: Vec<String> = items
            .into_iter()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty())
            .take(5)
            .collect();
        (criteria.len() >= 2).then_some(criteria)
    }
}
