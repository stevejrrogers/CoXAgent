//! `RunDevUseCase` — the DEV-BUG and DEV-FEATURE agents.
//!
//! The orchestrator owns claim/release: it atomically claims a ticket
//! (`Ready|Open -> InProgress` as `System`), runs the engine, and on success
//! completes it (`-> Done` / `-> Fixed`) while bumping the version. The agent
//! only does the coding; state moves are code, not prompt.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::selection::{next_open_bug, next_ready_feature};
use crate::{prompts, state::ProjectState};
use coxagent_domain::{Bump, Role, Status, TicketId};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Which developer role to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevMode {
    /// Fix the highest-priority open bug (`Open -> InProgress -> Fixed`, patch bump).
    Bug,
    /// Implement the next ready feature (`Ready -> InProgress -> Done`, minor bump).
    Feature,
}

impl DevMode {
    fn role(self) -> Role {
        match self {
            DevMode::Bug => Role::DevBug,
            DevMode::Feature => Role::DevFeature,
        }
    }

    fn bump(self) -> Bump {
        match self {
            DevMode::Bug => Bump::Patch,
            DevMode::Feature => Bump::Minor,
        }
    }

    fn complete_status(self) -> Status {
        match self {
            DevMode::Bug => Status::Fixed,
            DevMode::Feature => Status::Done,
        }
    }
}

/// Runs one developer pass. Returns the completed ticket id, or `None` when
/// there was nothing to do.
pub struct RunDevUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    mode: DevMode,
}

impl<S: StateStorePort, E: AgentEnginePort> RunDevUseCase<S, E> {
    pub fn new(
        store: Arc<S>,
        engine: Arc<E>,
        config: Config,
        work_dir: PathBuf,
        mode: DevMode,
    ) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            mode,
        }
    }

    /// Execute one developer pass.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or an unexpected state transition error.
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        let mut state = self.store.load().await?;
        let Some(id) = self.pick(&state) else {
            return Ok(None);
        };

        // Claim: System moves the ticket into progress and we persist before
        // running, so a crash leaves a recoverable in-progress claim.
        transition(&mut state, &id, Role::System, Status::InProgress)?;
        self.store.save(&state).await?;

        let outcome = self.engine.run(self.build_request(&state, &id)).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "{:?} engine failed on {id}: {}",
                self.mode,
                outcome.stderr.trim()
            ))
            .into());
        }

        // Complete: reload (the run may have changed nothing we track), move to
        // the terminal status, bump the version, persist.
        let mut state = self.store.load().await?;
        transition(
            &mut state,
            &id,
            self.mode.role(),
            self.mode.complete_status(),
        )?;
        state.current_version = state.current_version.bumped(self.mode.bump());
        self.store.save(&state).await?;
        Ok(Some(id))
    }

    fn pick(&self, state: &ProjectState) -> Option<TicketId> {
        match self.mode {
            DevMode::Bug => next_open_bug(state),
            DevMode::Feature => next_ready_feature(state),
        }
    }

    fn build_request(&self, state: &ProjectState, id: &TicketId) -> AgentRequest {
        let title = state.ticket(id).map_or("", coxagent_domain::Ticket::title);
        let _choice = self.config.engine.resolve(self.mode.role());
        AgentRequest {
            role: self.mode.role(),
            system_prompt: prompts::system_prompt(prompts::DEV),
            task_prompt: format!("Ticket {id}: {title}\n\nImplement it now."),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(3600),
        }
    }
}

/// Apply a guarded transition to a ticket in state, mapping a missing ticket to
/// a corruption error.
fn transition(
    state: &mut ProjectState,
    id: &TicketId,
    actor: Role,
    to: Status,
) -> Result<(), AppError> {
    let ticket = state
        .ticket_mut(id)
        .ok_or_else(|| PortError::Corrupt(format!("ticket {id} vanished mid-cycle")))?;
    ticket.transition_to(actor, to)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::AgentOutcome;
    use crate::selection::next_ready_feature;
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
                stdout: "changed foo.rs".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
            })
        }
    }

    fn ready_feature(id: &str) -> Ticket {
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
        t
    }

    #[tokio::test]
    async fn feature_dev_completes_and_bumps_minor() {
        let store = Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                tickets: vec![ready_feature("FEAT-001")],
                ..ProjectState::default()
            }),
        });
        let uc = RunDevUseCase::new(
            Arc::clone(&store),
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            DevMode::Feature,
        );
        let done = uc.execute().await.expect("run");
        assert_eq!(done.expect("some").as_str(), "FEAT-001");

        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Done);
        assert_eq!(state.current_version.to_string(), "0.1.0");
        assert!(next_ready_feature(&state).is_none());
    }

    #[tokio::test]
    async fn returns_none_when_no_work() {
        let store = Arc::new(MemStore::default());
        let uc = RunDevUseCase::new(
            store,
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            DevMode::Feature,
        );
        assert!(uc.execute().await.expect("run").is_none());
    }
}
