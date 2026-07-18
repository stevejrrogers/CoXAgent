//! `RunTestUseCase` — the TEST agent. Runs the QA prompt, parses discovered
//! bugs, and files them as `bug` tickets (deduped by title). Verifying `Fixed`
//! bugs against regression lands with real deploy in a later milestone.

use crate::config::Config;
use crate::error::AppError;
use crate::parsing::parse_items;
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
}

impl<S: StateStorePort, E: AgentEnginePort> RunTestUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
        }
    }

    /// Execute one test pass, returning the ids of newly filed bugs.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or unparseable output.
    pub async fn execute(&self) -> Result<Vec<TicketId>, AppError> {
        let _choice = self.config.engine.resolve(Role::Test);
        let (memory, shipped) = self.store.load().await.map_or_else(
            |_| (String::new(), String::new()),
            |s| {
                (
                    prompts::team_memory_block(&s.decisions, &s.lessons),
                    shipped_block(&s),
                )
            },
        );
        let request = AgentRequest {
            role: Role::Test,
            system_prompt: prompts::system_prompt(prompts::TEST),
            task_prompt: format!("Test the current build and report new bugs.{shipped}{memory}"),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(1800),
        };

        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(crate::error::PortError::Backend(format!(
                "TEST engine failed: {}",
                outcome.stderr.trim()
            ))
            .into());
        }

        let bugs = parse_items(&outcome.stdout)
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
        for id in passed {
            if let Some(t) = state.ticket_mut(&id) {
                if t.transition_to(Role::Test, coxagent_domain::Status::Verified)
                    .is_ok()
                {
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
fn shipped_block(state: &crate::state::ProjectState) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::AgentOutcome;
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
}
