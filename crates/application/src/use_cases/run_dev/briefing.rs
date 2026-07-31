// Part of the run_dev module split by concern — see run_dev/mod.rs.
#![allow(clippy::wildcard_imports)]
//! What a developer run is TOLD: the ticket brief, prior attempts, and the
//! knowledge the rest of the team already wrote down.

use super::*;

impl<S: StateStorePort, E: AgentEnginePort> RunDevUseCase<S, E> {
    /// Everything already written down about this ticket's subject: the team's
    /// own wiki, the project's docs, and closed tickets with the same symptom.
    /// A person walking into unfamiliar code reads these before typing; an
    /// agent only reads what the brief hands it.
    pub(super) async fn knowledge_brief(
        files: Option<&dyn crate::ports::outbound::WorkspaceFilesPort>,
        state: &ProjectState,
        id: &TicketId,
        ticket: Option<&coxagent_domain::Ticket>,
        work_dir: &std::path::Path,
    ) -> String {
        let query = ticket.map_or_else(String::new, |t| {
            format!(
                "{} {} {}",
                t.title(),
                t.description(),
                t.design()
                    .technical
                    .as_ref()
                    .map_or("", |d| d.approach.as_str())
            )
        });
        if query.trim().is_empty() {
            return String::new();
        }
        prompts::knowledge_block(
            files,
            &state.docs,
            &state.tickets,
            work_dir,
            &query,
            &id.to_string(),
        )
        .await
    }
    /// How previous attempts are briefed to the next one. Structured records
    /// name the gate and the files; a ticket that failed before that log
    /// existed falls back to its prose journal.
    pub(super) fn attempts_brief(state: &ProjectState, id: &TicketId) -> String {
        let failures = state.attempt_failures(&id.to_string());
        if failures.is_empty() {
            return state
                .ticket_journal
                .get(&id.to_string())
                .filter(|notes| !notes.is_empty())
                .map(|notes| {
                    format!(
                        "\n\nPREVIOUS ATTEMPTS on this ticket — build on these, do not repeat \
                         them:\n- {}",
                        notes.join("\n- ")
                    )
                })
                .unwrap_or_default();
        }
        let lines: Vec<String> = failures
            .iter()
            .map(|f| {
                let where_ = if f.files.is_empty() {
                    String::new()
                } else {
                    format!(" [in {}]", f.files.join(", "))
                };
                format!(
                    "attempt {} — rejected by {} ({:?}): {}{where_}",
                    f.attempt, f.gate, f.layer, f.detail
                )
            })
            .collect();
        format!(
            "\n\nPREVIOUS ATTEMPTS on this ticket — each was rejected by a specific gate. \
             Clear THAT, do not start over:\n- {}",
            lines.join("\n- ")
        )
    }
    #[allow(clippy::too_many_lines)] // one linear prompt assembly; splitting hurts readability
    pub(super) async fn build_request(&self, state: &ProjectState, id: &TicketId) -> AgentRequest {
        let ticket = state.ticket(id);
        let title = ticket.map_or("", coxagent_domain::Ticket::title);
        let _choice = self.config.engine.resolve(self.mode.role());
        let stack = prompts::stack_constraints(&self.config.architecture);
        let deploy = prompts::deploy_constraints(&self.config.deploy);
        // A UI ticket also carries the project design system into the prompt.
        let design = if ticket.is_some_and(coxagent_domain::Ticket::has_ui) {
            prompts::design_constraints(state.design_system.as_ref())
        } else {
            String::new()
        };
        let context_block = self
            .context
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(|c| format!("\n\n## Project context (goal, stack, scope, constraints):\n{c}\n"))
            .unwrap_or_default();
        // Human steering: recent USER comments on this ticket become explicit
        // instructions — commenting on an in-progress ticket steers the agent
        // on its next run instead of shouting into the void.
        let steering = {
            let notes: Vec<String> = state
                .comments
                .iter()
                .filter(|c| c.author == "USER" && c.ticket.as_deref() == Some(id.as_str()))
                .rev()
                .take(3)
                .map(|c| c.body.chars().take(400).collect::<String>())
                .collect();
            if notes.is_empty() {
                String::new()
            } else {
                format!(
                    "\n\nHUMAN STEERING on this ticket (newest first — follow it):\n- {}",
                    notes.join("\n- ")
                )
            }
        };
        let journal = Self::attempts_brief(state, id);
        // What was already done to this code. A human opens the file's history
        // before editing it; nothing in the ticket text carries that.
        let knowledge =
            Self::knowledge_brief(self.files.as_deref(), state, id, ticket, &self.work_dir).await;
        // Ask the BA rather than invent a requirement (and read any answer).
        let asking = prompts::ask_protocol_block(state, &id.to_string());
        let history = prompts::history_block(
            self.files.as_deref(),
            self.git.as_deref(),
            &self.work_dir,
            &format!(
                "{title} {}",
                ticket
                    .and_then(|t| t.design().technical.as_ref())
                    .map_or("", |d| d.approach.as_str())
            ),
        )
        .await;
        AgentRequest {
            role: self.mode.role(),
            // The system prompt stays BYTE-IDENTICAL across every DEV run of a
            // project: engines put it in the provider prompt cache, so a stable
            // prefix means cache READ pricing on back-to-back runs. Anything
            // per-ticket (stack/deploy/design blocks included — the design one
            // exists only for UI tickets) belongs in the task prompt below.
            system_prompt: prompts::system_prompt(prompts::DEV),
            task_prompt: format!(
                "Ticket {id}: {title}\n{}\nImplement it now.{stack}{deploy}{design}{context_block}{}{history}{knowledge}{}{}{}{steering}{journal}{asking}",
                ticket_brief(ticket),
                prompts::focus_block(
                    self.files.as_deref(),
                    &self.work_dir,
                    &format!(
                        "{title} {}",
                        ticket
                            .and_then(|t| t.design().technical.as_ref())
                            .map_or("", |d| d.approach.as_str())
                    ),
                )
                .await,
                prompts::repo_map_block(
                    self.files.as_deref(),
                    &self.work_dir,
                    self.config.workflow.token_saver,
                )
                .await,
                prompts::team_memory_block_relevant(
                    &state.decisions,
                    &state.lessons,
                    &format!(
                        "{title} {}",
                        ticket
                            .and_then(|t| t.design().technical.as_ref())
                            .map_or("", |d| d.approach.as_str())
                    ),
                ),
                prompts::hub_lessons_block(self.files.as_deref()).await,
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(3600),
            // Escalation: retries climb the ladder — and a LARGE ticket starts
            // on rung 1 outright. Experts don't try the cheap model first on
            // the hard problem and hope.
            escalation_level: {
                let attempts = state
                    .ticket_fail_attempts
                    .get(&id.to_string())
                    .copied()
                    .unwrap_or(0)
                    .min(3);
                let floor = u32::from(ticket.is_some_and(|t| {
                    t.complexity() == coxagent_domain::ticket::Complexity::Large
                }));
                u8::try_from(attempts.max(floor)).unwrap_or(3)
            },
        }
    }
}
