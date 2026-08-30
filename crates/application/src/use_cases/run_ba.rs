//! `RunBaUseCase` — the first agent slice. Composes the BA prompt, runs the
//! engine, parses the proposed features, and adds them to the backlog through
//! the same guarded `AddTicket` path. The agent proposes; code writes state.

use crate::config::Config;
use crate::error::AppError;
use crate::parsing::{normalize_title, parse_items, parse_items_lenient};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Role, TicketId, TicketType};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Runs the BA agent and appends its proposals to the backlog.
pub struct RunBaUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    context: String,
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunBaUseCase<S, E> {
    pub fn new(
        store: Arc<S>,
        engine: Arc<E>,
        config: Config,
        work_dir: PathBuf,
        context: String,
    ) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            context,
            files: None,
        }
    }

    /// Attach workspace file access for prompt context blocks; `None` (tests)
    /// reads as no context.
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    /// Execute the BA cycle, returning the ids of the tickets created.
    ///
    /// # Errors
    /// - [`AppError::Port`] when the engine fails or returns unparseable output.
    /// - [`AppError::Domain`] when a proposed feature is invalid.
    #[allow(clippy::too_many_lines)] // one linear pass; splitting hurts readability
    pub async fn execute(&self) -> Result<Vec<TicketId>, AppError> {
        let _choice = self.config.engine.resolve(Role::Ba);

        // Show the BA what already exists so it doesn't re-propose duplicates —
        // but BOUNDED: on a mature project the full list is thousands of tokens
        // re-sent every run. Active (unshipped) tickets matter most, newest
        // first, capped; the rest is a one-line count. Exact duplicates are
        // caught by the code-side title/semantic dedup regardless.
        let existing = self.store.load().await?;
        // Backpressure: a fat backlog means the constraint is DELIVERY, not
        // ideas. Proposing more just buries the board (91 pending happened).
        let pending = existing
            .tickets
            .iter()
            .filter(|t| {
                t.status() == coxagent_domain::Status::Pending
                    && matches!(
                        t.ticket_type(),
                        coxagent_domain::TicketType::Feature | coxagent_domain::TicketType::Chore
                    )
            })
            .count();
        if pending > self.config.workflow.backlog_cap() {
            return Ok(Vec::new());
        }
        let active: Vec<String> = existing
            .tickets
            .iter()
            .rev()
            .filter(|t| {
                use coxagent_domain::Status;
                matches!(
                    t.status(),
                    Status::Pending | Status::Ready | Status::InProgress | Status::Open
                )
            })
            .map(|t| format!("- {} {}", t.id(), t.title()))
            .collect();
        let total = existing.tickets.len();
        let shown = active.len().min(80);
        let backlog_block = if total == 0 {
            "(empty — this is a fresh project)".to_owned()
        } else {
            format!(
                "{}\n(… {total} tickets exist in total, {} active — assume anything obvious \
                 has been proposed already)",
                active[..shown].join("\n"),
                active.len()
            )
        };
        let taken: std::collections::HashSet<String> = existing
            .tickets
            .iter()
            .filter(|t| t.status() != coxagent_domain::Status::Rejected)
            .map(|t| normalize_title(t.title()))
            .collect();

        // The product surface, by name: the Wiki documents every shipped
        // feature, so its page titles ARE the feature inventory. Without this
        // list the BA re-invented existing surfaces as parallel features
        // (CXA-F233 built a second Activity feed instead of improving the
        // first) — it wasn't dumb, it was blind.
        let surface: Vec<String> = existing
            .docs
            .iter()
            .map(|d| format!("- {}", d.title))
            .take(60)
            .collect();
        let surface_block = if surface.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nPRODUCT SURFACE — features that already exist (the Wiki's page \
                 titles). PREFER proposing an IMPROVEMENT to one of these over a \
                 parallel new feature. A brand-new screen or menu is allowed only \
                 when extending the closest existing surface genuinely cannot work — \
                 and then your description MUST name that surface and say in one \
                 sentence why extension is not enough:\n{}",
                surface.join("\n")
            )
        };
        let knowledge = prompts::knowledge_block(
            self.files.as_deref(),
            &existing.docs,
            &existing.tickets,
            &self.work_dir,
            &format!("{} {}", self.context, existing.sprint_goal),
            "",
        )
        .await;
        // Hoisted out of the task_prompt's format! args (clippy: format in
        // format args) — same single allocation the inner format! made.
        let backlog_and_surface = format!("{backlog_block}{surface_block}");
        let request = AgentRequest {
            role: Role::Ba,
            system_prompt: prompts::system_prompt(prompts::BA),
            task_prompt: format!(
                "Product goal:\n{}\n{}\nEXISTING BACKLOG — do NOT re-propose anything already \
                 here (same or similar title/scope):\n{}\n\nPropose only genuinely NEW features \
                 that are not already covered above. When the project already has code (see the \
                 repo map below), propose features that fit the existing stack and structure — \
                 concrete, grounded in what's there, not generic.{}{}{}",
                self.context,
                sprint_goal_block(&existing.sprint_goal),
                backlog_and_surface,
                prompts::repo_map_block(self.files.as_deref(), &self.work_dir, true).await,
                knowledge,
                prompts::team_memory_block(&existing.decisions, &existing.lessons)
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(600),
            escalation_level: 0,
            label: None,
        };

        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(crate::error::PortError::Backend(format!(
                "BA engine exited with {:?}: {}",
                outcome.exit_code,
                outcome.failure_detail()
            ))
            .into());
        }

        // One cheap repair pass instead of discarding the whole call on a
        // malformed bracket. But a truly empty stdout (the opencode timeout
        // case) has nothing worth repairing — skip the 120s SM call entirely.
        let proposals = match parse_items(&outcome.stdout) {
            Ok(p) => p,
            Err(first) => {
                let stdout_is_blank = outcome.stdout.trim().is_empty()
                    || outcome
                        .stdout
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .count()
                        == 0;
                if stdout_is_blank {
                    return Err(crate::error::PortError::Corrupt(
                        "BA produced no output".to_owned(),
                    )
                    .into());
                }
                let fixed = crate::use_cases::repair_json(
                    self.engine.as_ref(),
                    &outcome.stdout,
                    "a JSON array of feature proposals",
                    &self.work_dir,
                )
                .await;
                match fixed.as_deref().map(parse_items) {
                    Some(Ok(p)) => p,
                    _ => {
                        // Repair gave nothing usable. Try partial salvage: keep
                        // every object that parses on its own rather than throwing
                        // the whole (mostly-good) array away because one segment
                        // is corrupt.
                        match parse_items_lenient(&outcome.stdout) {
                            Ok(partial) if !partial.is_empty() => partial,
                            _ => {
                                return Err(crate::error::PortError::Corrupt(format!(
                                    "BA output: {first}"
                                ))
                                .into())
                            }
                        }
                    }
                }
            }
        };

        // PO goal gate: every proposal is judged against the PRODUCT GOAL in
        // one cheap call before any ticket exists. Off-goal work dies at the
        // door with a written reason instead of consuming SA/DEV budget.
        let proposals = self.po_goal_gate(proposals).await;

        // Titles already live on the board — the yardstick for "already
        // covered". Rejected ones are excluded so a re-proposal of a rejected
        // idea still gets filtered by the exact-title `seen` set below, not by
        // similarity to something the team already said no to.
        let active_titles: Vec<String> = existing
            .tickets
            .iter()
            .filter(|t| t.status() != coxagent_domain::Status::Rejected)
            .map(|t| t.title().to_owned())
            .collect();

        let adder = AddTicketUseCase::new(Arc::clone(&self.store));
        let mut created = Vec::with_capacity(proposals.len());
        let mut seen = taken;
        // Titles filed in THIS run, checked with the same fuzzy rule so a batch
        // of six paraphrases of one idea files exactly one.
        let mut created_titles: Vec<String> = Vec::new();
        for p in proposals {
            let key = normalize_title(&p.title);
            if key.is_empty() || !seen.insert(key) {
                continue;
            }
            // Semantic dedup: an exact-title check let paraphrases through —
            // "Bug triage and burn-down" vs "Bug burndown cadence — Q3 triage"
            // are different strings, same ticket. `duplicates_existing` folds in
            // Jaccard similarity and the single-slot rule for backlog-ceremony
            // tickets, against both the board and this run's own output.
            if crate::parsing::duplicates_existing(&p.title, &active_titles)
                || crate::parsing::duplicates_existing(&p.title, &created_titles)
            {
                continue;
            }
            created_titles.push(p.title.clone());
            let id = adder
                .execute(AddTicketInput {
                    ticket_type: TicketType::Feature,
                    title: p.title,
                    description: p.description,
                    priority: p.priority,
                    complexity: p.complexity,
                    has_ui: p.has_ui,
                    acceptance_criteria: p.acceptance_criteria,
                    goal: None,
                })
                .await?;
            created.push(id);
        }
        Ok(created)
    }

    /// One PO call judges all proposals against the product goal; returns the
    /// survivors. Any failure (engine down, unparseable) keeps ALL proposals —
    /// the gate can starve bad work, never good work.
    async fn po_goal_gate(
        &self,
        proposals: Vec<crate::parsing::ProposedItem>,
    ) -> Vec<crate::parsing::ProposedItem> {
        let goal = self.context.trim();
        if goal.is_empty() || proposals.is_empty() {
            return proposals;
        }
        let listing = proposals
            .iter()
            .enumerate()
            .map(|(i, p)| {
                format!(
                    "{i}. {} — {}",
                    p.title,
                    p.description.chars().take(200).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let request = AgentRequest {
            role: Role::Po,
            system_prompt: prompts::system_prompt(prompts::PO),
            task_prompt: format!(
                "GOAL GATE. The product goal/direction is:\n{goal}\n\nProposed tickets:\n{listing}\n\n\
                 For each index, does it DIRECTLY serve the stated goal (not merely 'generally \
                 useful')? Reply with ONLY a JSON array: \
                 [{{\"index\": number, \"verdict\": \"YES\"|\"NO\", \"reason\": string}}]"
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(180),
            escalation_level: 0,
            label: None,
        };
        let Ok(o) = self.engine.run(request).await else {
            return proposals;
        };
        if !o.succeeded() {
            return proposals;
        }
        let raw = &o.stdout;
        let (Some(a), Some(b)) = (raw.find('['), raw.rfind(']')) else {
            return proposals;
        };
        let Ok(verdicts) = serde_json::from_str::<Vec<serde_json::Value>>(&raw[a..=b]) else {
            return proposals;
        };
        let rejected: Vec<(usize, String)> = verdicts
            .iter()
            .filter(|v| v.get("verdict").and_then(serde_json::Value::as_str) == Some("NO"))
            .filter_map(|v| {
                Some((
                    usize::try_from(v.get("index")?.as_u64()?).ok()?,
                    v.get("reason")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("off-goal")
                        .to_owned(),
                ))
            })
            .collect();
        if rejected.is_empty() {
            let n = proposals.len();
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |st| {
                st.log_activity("PO", &format!("goal gate: {n}/{n} proposals aligned"), None);
                Ok(())
            })
            .await;
            return proposals;
        }
        let notes: Vec<String> = rejected
            .iter()
            .filter_map(|(i, r)| proposals.get(*i).map(|p| format!("'{}' — {}", p.title, r)))
            .collect();
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |st| {
            for n in &notes {
                st.post_comment("PO", &format!("🚫 Goal gate rejected: {n}"), None);
            }
            Ok(())
        })
        .await;
        let dead: std::collections::BTreeSet<usize> =
            rejected.into_iter().map(|(i, _)| i).collect();
        proposals
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !dead.contains(i))
            .map(|(_, p)| p)
            .collect()
    }
}

/// When the PO has set a sprint goal, steer the BA to break it into concrete
/// tickets that directly advance it — the top priority for the sprint.
fn sprint_goal_block(goal: &str) -> String {
    let g = goal.trim();
    if g.is_empty() {
        String::new()
    } else {
        format!(
            "\nTHIS SPRINT'S GOAL (set by the Product Owner) — prioritise proposals that directly \
             advance it, broken into concrete, buildable tickets:\n{g}\n"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
    use crate::state::ProjectState;
    use crate::PortError;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }

    #[async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
            state.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = state.clone();
            Ok(())
        }
    }

    struct CannedEngine {
        stdout: String,
        code: i32,
    }

    #[async_trait]
    impl AgentEnginePort for CannedEngine {
        fn id(&self) -> &'static str {
            "canned"
        }
        async fn run(&self, _req: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: self.stdout.clone(),
                stderr: String::new(),
                exit_code: Some(self.code),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
                engine: String::new(),
            })
        }
    }

    fn run(
        store: Arc<MemStore>,
        engine: Arc<CannedEngine>,
    ) -> RunBaUseCase<MemStore, CannedEngine> {
        RunBaUseCase::new(
            store,
            engine,
            Config::default(),
            PathBuf::from("/tmp"),
            "goal: a todo app".to_owned(),
        )
    }

    #[tokio::test]
    async fn ba_adds_proposed_features_to_backlog() {
        let store = Arc::new(MemStore::default());
        let engine = Arc::new(CannedEngine {
            stdout: r#"Here are ideas:
                [
                  {"title":"Login","description":"auth","priority":"high","complexity":"medium","has_ui":true},
                  {"title":"Export CSV","priority":"low","complexity":"small","has_ui":false}
                ]"#
            .to_owned(),
            code: 0,
        });
        let created = run(Arc::clone(&store), engine)
            .execute()
            .await
            .expect("run");
        assert_eq!(created.len(), 2);
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets.len(), 2);
        assert_eq!(state.tickets[0].title(), "Login");
        assert!(state.tickets[0].has_ui());
    }

    #[tokio::test]
    async fn ba_errors_when_engine_fails() {
        let store = Arc::new(MemStore::default());
        let engine = Arc::new(CannedEngine {
            stdout: String::new(),
            code: 1,
        });
        assert!(run(store, engine).execute().await.is_err());
    }

    #[tokio::test]
    async fn ba_errors_on_unparseable_output() {
        let store = Arc::new(MemStore::default());
        let engine = Arc::new(CannedEngine {
            stdout: "sorry, no JSON here".to_owned(),
            code: 0,
        });
        assert!(run(store, engine).execute().await.is_err());
    }

    #[tokio::test]
    async fn ba_salvages_partial_good_items_when_no_repair() {
        // One valid feature plus one garbage segment; repair offers no help, so
        // partial salvage must keep ONLY the valid item end-to-end.
        struct SalvageEngine {
            proposals: String,
        }
        #[async_trait]
        impl AgentEnginePort for SalvageEngine {
            fn id(&self) -> &'static str {
                "salvage"
            }
            async fn run(&self, req: AgentRequest) -> Result<AgentOutcome, PortError> {
                match req.role {
                    Role::Ba => Ok(AgentOutcome {
                        stdout: self.proposals.clone(),
                        stderr: String::new(),
                        exit_code: Some(0),
                        usage: None,
                        trace: String::new(),
                        session_id: None,
                        sandbox: SandboxStatus::default(),
                        engine: String::new(),
                    }),
                    // No repair available: any SM call fails outright, so
                    // repair_json yields None and we fall through to salvage.
                    _ => Err(PortError::Corrupt("no repair available".to_owned())),
                }
            }
        }
        let store = Arc::new(MemStore::default());
        let engine = Arc::new(SalvageEngine {
            proposals:
                "[{\"title\":\"Login\",\"priority\":\"high\",\"complexity\":\"medium\",\"has_ui\":true}, {\"title\":"
                    .to_owned(),
        });
        let uc = RunBaUseCase::new(
            Arc::clone(&store),
            engine,
            Config::default(),
            PathBuf::from("/tmp"),
            "goal: a todo app".to_owned(),
        );
        let created = uc.execute().await.expect("partial salvage should succeed");
        assert_eq!(created.len(), 1);
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets.len(), 1);
        assert_eq!(state.tickets[0].title(), "Login");
    }

    #[tokio::test]
    async fn ba_skips_repair_when_stdout_is_blank() {
        // Blank stdout (the opencode timeout case) must NOT trigger an SM repair
        // call — it returns a clear Corrupt error instead.
        struct BlankEngine;
        #[async_trait]
        impl AgentEnginePort for BlankEngine {
            fn id(&self) -> &'static str {
                "blank"
            }
            async fn run(&self, req: AgentRequest) -> Result<AgentOutcome, PortError> {
                // The only call allowed is the BA proposal; any SM repair attempt
                // would carry a different role and fail this assertion.
                assert_eq!(req.role, Role::Ba);
                Ok(AgentOutcome {
                    stdout: "   \n  ".to_owned(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    usage: None,
                    trace: String::new(),
                    session_id: None,
                    sandbox: SandboxStatus::default(),
                    engine: String::new(),
                })
            }
        }
        let store = Arc::new(MemStore::default());
        let engine = Arc::new(BlankEngine);
        let uc = RunBaUseCase::new(
            store,
            engine,
            Config::default(),
            PathBuf::from("/tmp"),
            "goal: a todo app".to_owned(),
        );
        match uc.execute().await {
            Err(crate::error::AppError::Port(crate::error::PortError::Corrupt(msg))) => {
                assert!(msg.contains("no output"), "unexpected message: {msg}");
            }
            other => panic!("expected Corrupt error for blank stdout, got {other:?}"),
        }
    }
}
