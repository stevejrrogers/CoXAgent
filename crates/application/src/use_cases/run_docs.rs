//! `RunDocsUseCase` — the DOCS agent. Documents one `Done` feature (writing a
//! user guide into the codebase) and moves it to `Documented`. Runs after TEST
//! so only completed work is documented.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::selection::documentable_candidates;
use coxagent_domain::{Role, Status, TicketId};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Runs one documentation pass.
pub struct RunDocsUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    worker: String,
    phase: Option<crate::use_cases::runner::PhaseReporter>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunDocsUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            worker: String::new(),
            phase: None,
        }
    }

    /// Set this runner's identity (`account@host`) so the DOCS stage is claimed
    /// per-ticket for parallel-safe documentation across concurrent runners.
    #[must_use]
    pub fn with_worker(mut self, worker: impl Into<String>) -> Self {
        self.worker = worker.into();
        self
    }

    /// Attach the live "working now" reporter; fired only after the stage is won.
    #[must_use]
    pub fn with_phase(mut self, phase: Option<crate::use_cases::runner::PhaseReporter>) -> Self {
        self.phase = phase;
        self
    }

    /// Document the next `Done` feature. Returns its id, or `None` when there's
    /// nothing to document.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or an unexpected transition error.
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        let state = self.store.load().await?;
        let worker = if self.worker.is_empty() {
            "local".to_owned()
        } else {
            self.worker.clone()
        };
        let now = crate::state::now_rfc3339();
        let mut chosen = None;
        for cand in documentable_candidates(&state) {
            if self.store.claim_stage(&cand, "docs", &worker, &now).await? {
                chosen = Some(cand);
                break;
            }
        }
        let Some(id) = chosen else {
            return Ok(None);
        };
        if let Some(p) = &self.phase {
            p(Some(("DOCS".to_owned(), id.to_string())));
        }
        let title = state
            .ticket(&id)
            .map_or("", coxagent_domain::Ticket::title)
            .to_owned();

        let _choice = self.config.engine.resolve(Role::Docs);
        let outcome = self
            .engine
            .run(AgentRequest {
                role: Role::Docs,
                system_prompt: prompts::system_prompt(prompts::DOCS),
                task_prompt: format!("Document feature {id}: {title}"),
                work_dir: self.work_dir.clone(),
                timeout: Duration::from_secs(900),
            })
            .await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "DOCS engine failed on {id}: {}",
                outcome.stderr.trim()
            ))
            .into());
        }

        let mut state = self.store.load().await?;
        let ticket = state
            .ticket_mut(&id)
            .ok_or_else(|| PortError::Corrupt(format!("ticket {id} vanished")))?;
        ticket.transition_to(Role::Docs, Status::Documented)?;
        self.store.save(&state).await?;
        Ok(Some(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::AgentOutcome;
    use crate::state::ProjectState;
    use coxagent_domain::{Complexity, Priority, TechnicalDesign, Ticket, TicketType};
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

    struct OkEngine;
    #[async_trait::async_trait]
    impl AgentEnginePort for OkEngine {
        fn id(&self) -> &'static str {
            "ok"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: "wrote docs".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
            })
        }
    }

    fn done_feature(id: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t");
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("d");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.transition_to(Role::DevFeature, Status::InProgress)
            .expect("claim");
        t.transition_to(Role::DevFeature, Status::Done)
            .expect("done");
        t
    }

    #[tokio::test]
    async fn documents_done_feature() {
        let store = Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                tickets: vec![done_feature("F001")],
                ..ProjectState::default()
            }),
        });
        let uc = RunDocsUseCase::new(
            Arc::clone(&store),
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
        );
        let id = uc.execute().await.expect("run");
        assert_eq!(id.expect("some").as_str(), "F001");
        assert_eq!(
            store.load().await.expect("load").tickets[0].status(),
            Status::Documented
        );
    }

    #[tokio::test]
    async fn nothing_to_document_is_none() {
        let store = Arc::new(MemStore::default());
        let uc = RunDocsUseCase::new(
            store,
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
        );
        assert!(uc.execute().await.expect("run").is_none());
    }
}
