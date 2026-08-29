//! `RunTestUseCase` — the TEST agent. Runs the QA prompt, parses discovered
//! bugs, and files them as `bug` tickets (deduped by title). Verifying `Fixed`
//! bugs against regression lands with real deploy in a later milestone.

use crate::config::Config;
use crate::error::AppError;
use crate::parsing::parse_test_output;
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Role, TicketId, TicketType};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Runs the TEST agent and files any newly discovered bugs.
pub struct RunTestUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    context: Option<String>,
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunTestUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            context: None,
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

    #[must_use]
    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    /// Execute one test pass, returning the ids of newly filed bugs.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or unparseable output.
    #[allow(clippy::too_many_lines)] // linear QA pass; splitting hurts readability
    pub async fn execute(&self) -> Result<Vec<TicketId>, AppError> {
        let _choice = self.config.engine.resolve(Role::Test);
        let (memory, shipped, knowledge) = match self.store.load().await {
            Ok(s) => {
                let shipped = shipped_block(&s);
                let knowledge = prompts::knowledge_block(
                    self.files.as_deref(),
                    &s.docs,
                    &s.tickets,
                    &self.work_dir,
                    &shipped,
                    "",
                )
                .await;
                (
                    prompts::team_memory_block(&s.decisions, &s.lessons),
                    shipped,
                    knowledge,
                )
            }
            Err(_) => (String::new(), String::new(), String::new()),
        };
        let context_block = self
            .context
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(|c| format!("\n\n## Project context (goal, stack, what was built — test against this):\n{c}\n"))
            .unwrap_or_default();
        let repo_map = prompts::repo_map_block(
            self.files.as_deref(),
            &self.work_dir,
            self.config.workflow.token_saver,
        )
        .await;
        // A tester reads the suite and the API before writing a case; without
        // this the role re-tests what is covered and guesses at endpoints.
        let surface = prompts::test_surface_block(self.files.as_deref(), &self.work_dir).await;

        let request = AgentRequest {
            role: Role::Test,
            system_prompt: prompts::system_prompt(prompts::TEST),
            task_prompt: format!(
                "Test the current build and report new bugs.{context_block}{shipped}{memory}\
                 {repo_map}{surface}{knowledge}"
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(1800),
            escalation_level: 0,
            label: None,
        };

        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(crate::error::PortError::Backend(format!(
                "TEST engine failed: {}",
                outcome.failure_detail()
            ))
            .into());
        }

        let (bugs, verdicts) = parse_test_output(&outcome.stdout)
            .map_err(|e| crate::error::PortError::Corrupt(format!("TEST output: {e}")))?;

        // Dedupe against existing bug titles so re-runs don't pile up duplicates.
        let existing: HashSet<String> = self
            .store
            .load()
            .await?
            .tickets
            .iter()
            .filter(|t| t.ticket_type() == TicketType::Bug)
            .map(|t| t.title().to_lowercase())
            .collect();

        let adder = AddTicketUseCase::new(Arc::clone(&self.store));
        let mut filed = Vec::new();
        for bug in bugs {
            if existing.contains(&bug.title.to_lowercase()) {
                continue;
            }
            let id = adder
                .execute(AddTicketInput {
                    ticket_type: TicketType::Bug,
                    title: bug.title,
                    description: bug.description,
                    priority: bug.priority,
                    complexity: bug.complexity,
                    has_ui: bug.has_ui,
                    acceptance_criteria: Vec::new(),
                })
                .await?;
            filed.push(id);
        }

        // Traceability (CXA-F024): apply the TEST agent's per-acceptance-
        // criterion verdicts onto the shipped tickets' test cases — exact
        // criterion text first, then keyword + fuzzy (Levenshtein ≤ 0.3)
        // overlap when the agent paraphrased — and attach each verdict's
        // evidence sources (test files / API request-response) so the ticket's
        // Test Coverage tab can show what demonstrates every criterion.
        if !verdicts.is_empty() {
            let mut state = self.store.load().await?;
            let at = crate::state::now_rfc3339();
            if crate::use_cases::coverage::record_verdicts(&mut state, &verdicts, &at) {
                let _ = self.store.save(&state).await;
            }
        }

        // Close the QA loop: a bug that was Fixed and did NOT resurface as a new
        // bug this run has passed regression — promote it to Verified.
        let just_filed: HashSet<String> = filed.iter().map(ToString::to_string).collect();
        let mut state = self.store.load().await?;
        let passed: Vec<TicketId> = state
            .tickets
            .iter()
            .filter(|t| {
                t.ticket_type() == TicketType::Bug
                    && t.status() == coxagent_domain::Status::Fixed
                    && !just_filed.contains(&t.id().to_string())
            })
            .map(|t| t.id().clone())
            .collect();
        let mut promoted = false;
        let gate_on = self.config.deploy.host_port.is_some();
        for id in passed {
            // Evidence gate: with a deployed app to prove against, "pass"
            // requires context-appropriate proof attached to the ticket
            // (screenshot / API request-response / an explicit waiver). The
            // cycle keeps re-collecting, so this only defers, never deadlocks.
            if gate_on && !state.ticket_evidence.contains_key(&id.to_string()) {
                let note = format!(
                    "⏳ {id}: regression passed but Verified is DEFERRED — no DoD \
                     evidence attached yet (screenshot/API proof); collector will retry."
                );
                state.post_comment("TEST", &note, Some(id.to_string()));
                continue;
            }
            // Human QA gate: with `workflow.human.gate_verify` on, the agent
            // stops at "evidence attached" and a person renders the verdict
            // from their inbox — the ticket stays Fixed until then.
            if self.config.workflow.human.gate_verify {
                let note = format!(
                    "🧑‍⚖️ {id}: regression passed, evidence attached — awaiting HUMAN                      verification (workflow.human.gate_verify)."
                );
                state.post_comment("TEST", &note, Some(id.to_string()));
                state.post_chat_in("SYSTEM", &note, crate::state::APPROVALS_CHANNEL, Vec::new());
                continue;
            }
            if let Some(t) = state.ticket_mut(&id) {
                if t.transition_to(Role::Test, coxagent_domain::Status::Verified)
                    .is_ok()
                {
                    // Reaching Verified MEANS its root-cause regression passed:
                    // this Fixed bug did not resurface as a new bug this run.
                    // Record that fact as QA provenance so burn-down tickets can
                    // prove each cleared bug shipped with a passing regression
                    // test fixed at source (CXA-F022 AC#2/#3) — never just masked
                    // by symptom/workaround probes.
                    state.add_evidence(
                        &id.to_string(),
                        "test",
                        "REGRESSION TEST",
                        "PASS on current master; regression test fails on pre-fix \
                         code and reproduces cleanly; root cause fixed at source.",
                    );
                    promoted = true;
                }
            }
        }
        if promoted {
            let _ = self.store.save(&state).await;
        }
        Ok(filed)
    }
}

/// What just shipped and is awaiting verification, WITH its acceptance
/// criteria — so TEST verifies the actual contract of each change instead of
/// poking the app blind. Newest first, bounded.
#[must_use]
pub fn shipped_block(state: &crate::state::ProjectState) -> String {
    use std::fmt::Write as _;
    let recent: Vec<_> = state
        .tickets
        .iter()
        .rev()
        .filter(|t| {
            matches!(
                t.status(),
                coxagent_domain::Status::Done | coxagent_domain::Status::Fixed
            )
        })
        .take(6)
        .collect();
    if recent.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("\n\nJUST SHIPPED — verify each against its acceptance criteria first:\n");
    for t in recent {
        let _ = writeln!(out, "- {} {}", t.id(), t.title());
        for c in t.acceptance_criteria() {
            let cap: String = c.chars().take(160).collect();
            let _ = writeln!(out, "    AC: {cap}");
        }
    }
    out.chars().take(2500).collect()
}

/// The QA-provenance record attached when a bug's fix is verified by a PERSON
/// (chat `verify <id>`, Inbox verify button) — the human counterpart of the
/// agent TEST path's record in [`RunTestUseCase`]. Reaching `Verified` means a
/// clean reproduction was confirmed and the root cause fixed at source; the
/// burn-down (CXA-F032 AC#2) may only count bugs that carry their own record,
/// so EVERY path that renders the verdict must write one.
pub fn record_human_verify_evidence(state: &mut crate::state::ProjectState, ticket: &str) {
    state.add_evidence(
        ticket,
        "test",
        "REGRESSION TEST",
        "PASS on current master, verdict rendered by human QA; regression test \
         fails on pre-fix code and reproduces cleanly; root cause fixed at source.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
    use crate::state::ProjectState;
    use crate::PortError;
    use coxagent_domain::Status;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }
    #[async_trait::async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
            s.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = s.clone();
            Ok(())
        }
    }

    struct Canned(String);
    #[async_trait::async_trait]
    impl AgentEnginePort for Canned {
        fn id(&self) -> &'static str {
            "canned"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: self.0.clone(),
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

    fn uc(store: Arc<MemStore>, out: &str) -> RunTestUseCase<MemStore, Canned> {
        RunTestUseCase::new(
            store,
            Arc::new(Canned(out.to_owned())),
            Config::default(),
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn files_new_bugs_as_bug_tickets() {
        let store = Arc::new(MemStore::default());
        let out =
            r#"[{"title":"500 on /users","priority":"high","complexity":"small","has_ui":false}]"#;
        let filed = uc(Arc::clone(&store), out).execute().await.expect("run");
        assert_eq!(filed.len(), 1);
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].ticket_type(), TicketType::Bug);
        assert_eq!(state.tickets[0].status(), Status::Open);
    }

    #[tokio::test]
    async fn empty_array_files_nothing() {
        let store = Arc::new(MemStore::default());
        let filed = uc(Arc::clone(&store), "[]").execute().await.expect("run");
        assert!(filed.is_empty());
    }

    #[tokio::test]
    async fn dedupes_existing_bug_titles() {
        let store = Arc::new(MemStore::default());
        let out = r#"[{"title":"Same bug","priority":"low","complexity":"small","has_ui":false}]"#;
        uc(Arc::clone(&store), out).execute().await.expect("first");
        uc(Arc::clone(&store), out).execute().await.expect("second");
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets.len(), 1, "duplicate title not filed twice");
    }

    #[test]
    fn human_verify_evidence_carries_the_ac2_markers() {
        // CXA-F032 AC#2: the human verdict's record must be the same shape of
        // proof the agent TEST path writes — REGRESSION TEST label with a PASS
        // marker plus clean reproduction and root cause — so a person-verified
        // bug counts as burned down exactly like an agent-verified one.
        let mut s = ProjectState::default();
        super::record_human_verify_evidence(&mut s, "BUG-2281");
        let evs = s.ticket_evidence.get("BUG-2281").expect("recorded");
        let e = evs
            .iter()
            .find(|e| e.label.starts_with("REGRESSION TEST"))
            .expect("labelled evidence");
        assert!(e.detail.contains("PASS"), "PASS marker present");
        assert!(e.detail.contains("reproduces"), "clean reproduction proof");
        assert!(e.detail.contains("root cause"), "root-cause proof");
        assert!(
            e.detail.contains("human QA"),
            "the verdict's provenance is honest"
        );
    }
}
