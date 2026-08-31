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
        let steering = prompts::human_steering_block(state, id.as_str());
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
        // Tickets designed before a refactor can name files that no longer
        // exist. Say so mechanically — a DEV chasing a dead path burns a whole
        // attempt before it asks.
        let stale_design = {
            let listed: Vec<&str> = ticket
                .and_then(|t| t.design().technical.as_ref())
                .map(|d| d.files.iter().map(String::as_str).collect())
                .unwrap_or_default();
            let mut missing: Vec<&str> = Vec::new();
            if let Some(fs) = self.files.as_deref() {
                for f in listed {
                    if !f.trim().is_empty() && fs.stat(&self.work_dir.join(f)).await.is_none() {
                        missing.push(f);
                    }
                }
            }
            if missing.is_empty() {
                String::new()
            } else {
                format!(
                    "\nWARNING: this design predates a refactor — these listed files no \
                     longer exist: {}. Locate the moved code via `.coxagent/REPO_MAP.md` \
                     (or ask SA) before implementing; do NOT recreate the old files.",
                    missing.join(", ")
                )
            }
        };
        // Orientation block: the facts every DEV session otherwise SPENDS API
        // rounds discovering by hand (`git status`, `ls`, probing the design's
        // files one by one — four exploratory rounds observed per session,
        // each replaying the whole context). Computed here for the cost of a
        // few port calls, so the FIRST model turn is already oriented.
        let orientation = {
            use std::fmt::Write as _;
            let mut s = String::new();
            if let Some(git) = &self.git {
                let (ok, branch) = git
                    .raw(&self.work_dir, &["rev-parse", "--abbrev-ref", "HEAD"])
                    .await;
                if ok {
                    let _ = write!(s, "\n## Workspace orientation\nbranch: {}", branch.trim());
                }
                let (ok, status) = git.raw(&self.work_dir, &["status", "--porcelain"]).await;
                if ok {
                    let lines: Vec<&str> = status.lines().take(20).collect();
                    if lines.is_empty() {
                        s.push_str("\nworking tree: clean");
                    } else {
                        let _ = write!(s, "\nworking tree (dirty):\n{}", lines.join("\n"));
                    }
                }
            }
            let listed: Vec<String> = ticket
                .and_then(|t| t.design().technical.as_ref())
                .map(|d| d.files.clone())
                .unwrap_or_default();
            if let Some(fs) = self.files.as_deref() {
                for f in listed.iter().filter(|f| !f.trim().is_empty()).take(12) {
                    match fs.stat(&self.work_dir.join(f)).await {
                        Some(m) => {
                            let _ = write!(s, "\ndesign file {f}: exists, {} bytes", m.size);
                        }
                        None => {
                            let _ = write!(s, "\ndesign file {f}: MISSING");
                        }
                    }
                }
            }
            if s.is_empty() {
                s
            } else {
                s.push_str(
                    "\nTrust this block instead of re-running ls/git status to orient yourself.\n",
                );
                s
            }
        };
        // The repo map exists to ORIENT a session that has no target; when the
        // SA design already names real files, the map is dead weight replayed
        // into every API round of the session — skip it.
        let design_names_real_files = ticket
            .and_then(|t| t.design().technical.as_ref())
            .is_some_and(|d| d.files.iter().any(|f| !f.trim().is_empty()))
            && stale_design.is_empty();
        AgentRequest {
            role: self.mode.role(),
            // The system prompt stays BYTE-IDENTICAL across every DEV run of a
            // project: engines put it in the provider prompt cache, so a stable
            // prefix means cache READ pricing on back-to-back runs. Anything
            // per-ticket (stack/deploy/design blocks included — the design one
            // exists only for UI tickets) belongs in the task prompt below.
            system_prompt: prompts::system_prompt(prompts::DEV),
            task_prompt: format!(
                "Ticket {id}: {title}\n{}{stale_design}{orientation}\nImplement it now.{stack}{deploy}{design}{context_block}{}{history}{knowledge}{}{}{}{steering}{journal}{asking}{}",
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
                if design_names_real_files {
                    String::new()
                } else {
                    prompts::repo_map_block(
                        self.files.as_deref(),
                        &self.work_dir,
                        self.config.workflow.token_saver,
                    )
                    .await
                },
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
                prompts::BRIEF_PROTOCOL,
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
            label: Some(id.to_string()),
        }
    }
}
