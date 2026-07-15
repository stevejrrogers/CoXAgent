//! `RunPdUseCase` — the PD (Product Designer) design gate. Picks the top
//! pending UI feature that has a technical design but no UX yet, runs the engine
//! to author the UX design, attaches it, and moves the ticket to `ready`. The
//! aggregate re-checks Definition of Ready, so a UI ticket only advances once
//! both technical and UX designs exist — enforced by code, not the prompt.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::selection::next_feature_needing_ux;
use coxagent_domain::{Role, Status, TicketId, UxDesign};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The PD's JSON output: a UX design for one UI feature.
#[derive(Debug, Deserialize)]
struct UxOutput {
    #[serde(default)]
    user_flow: String,
    #[serde(default)]
    screens: Vec<String>,
    #[serde(default)]
    component_states: Vec<String>,
    #[serde(default)]
    responsive_notes: String,
}

/// Runs one PD UX-design pass.
pub struct RunPdUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    worker: String,
}

impl<S: StateStorePort, E: AgentEnginePort> RunPdUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            worker: String::new(),
        }
    }

    /// Set this runner's identity (`account@host`) so the PD stage is claimed
    /// per-ticket for parallel-safe UX design across concurrent runners.
    #[must_use]
    pub fn with_worker(mut self, worker: impl Into<String>) -> Self {
        self.worker = worker.into();
        self
    }

    /// Author UX for the next pending UI feature awaiting it. Returns the
    /// readied ticket id, or `None` when nothing needs UX.
    ///
    /// # Errors
    /// [`AppError`] on engine failure, unparseable output, or a DoR violation.
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        let state = self.store.load().await?;
        let Some(id) = next_feature_needing_ux(&state) else {
            return Ok(None);
        };
        let worker = if self.worker.is_empty() {
            "local".to_owned()
        } else {
            self.worker.clone()
        };
        if !self
            .store
            .claim_stage(&id, "pd", &worker, &crate::state::now_rfc3339())
            .await?
        {
            return Ok(None);
        }
        let title = state
            .ticket(&id)
            .map_or("", coxagent_domain::Ticket::title)
            .to_owned();

        let outcome = self.engine.run(self.build_request(&id, &title)).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "PD engine failed on {id}: {}",
                outcome.stderr.trim()
            ))
            .into());
        }
        let ux =
            parse_ux(&outcome.stdout).map_err(|e| PortError::Corrupt(format!("PD output: {e}")))?;

        let mut state = self.store.load().await?;
        let ticket = state
            .ticket_mut(&id)
            .ok_or_else(|| PortError::Corrupt(format!("ticket {id} vanished")))?;
        ticket.set_ux_design(Role::Pd, ux_of(&ux))?;
        // DoR re-checked here; passes now that both technical and UX exist.
        ticket.transition_to(Role::Pd, Status::Ready)?;
        self.store.save(&state).await?;
        Ok(Some(id))
    }

    fn build_request(&self, id: &TicketId, title: &str) -> AgentRequest {
        let _choice = self.config.engine.resolve(Role::Pd);
        AgentRequest {
            role: Role::Pd,
            system_prompt: prompts::system_prompt(prompts::PD),
            task_prompt: format!("Design the UX for feature {id}: {title}"),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(1200),
        }
    }
}

fn parse_ux(raw: &str) -> Result<UxOutput, String> {
    let start = raw.find('{').ok_or("no JSON object found")?;
    let end = raw.rfind('}').ok_or("no closing brace")?;
    if end < start {
        return Err("malformed object bounds".to_owned());
    }
    serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())
}

fn ux_of(u: &UxOutput) -> UxDesign {
    UxDesign {
        user_flow: u.user_flow.clone(),
        screens: u.screens.clone(),
        component_states: u.component_states.clone(),
        responsive_notes: u.responsive_notes.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, AgentRequest};
    use crate::state::ProjectState;
    use crate::use_cases::{AddTicketInput, AddTicketUseCase};
    use coxagent_domain::{Complexity, Priority, TechnicalDesign, TicketType};
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
            })
        }
    }

    /// Seed a UI feature that already has a technical design (SA done), pending.
    async fn seed_ui_with_technical(store: &Arc<MemStore>) -> TicketId {
        AddTicketUseCase::new(Arc::clone(store))
            .execute(AddTicketInput {
                ticket_type: TicketType::Feature,
                title: "UI feature".to_owned(),
                description: String::new(),
                priority: Priority::High,
                complexity: Complexity::Small,
                has_ui: true,
                acceptance_criteria: Vec::new(),
            })
            .await
            .expect("seed");
        let mut s = store.load().await.expect("load");
        let id = s.tickets[0].id().clone();
        s.tickets[0]
            .set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("tech");
        store.save(&s).await.expect("save");
        id
    }

    fn uc(store: Arc<MemStore>, out: &str) -> RunPdUseCase<MemStore, Canned> {
        RunPdUseCase::new(
            store,
            Arc::new(Canned(out.to_owned())),
            Config::default(),
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn authors_ux_and_readies_ui_feature() {
        let store = Arc::new(MemStore::default());
        seed_ui_with_technical(&store).await;
        let out = r#"{"user_flow":"open, type, send","screens":["compose"],"component_states":["empty","sending"],"responsive_notes":"stacks on mobile"}"#;
        let id = uc(Arc::clone(&store), out).execute().await.expect("run");
        assert!(id.is_some());
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Ready);
        let ux = state.tickets[0].design().ux.as_ref().expect("ux");
        assert_eq!(ux.screens, vec!["compose".to_owned()]);
    }

    #[tokio::test]
    async fn nothing_needing_ux_returns_none() {
        let store = Arc::new(MemStore::default());
        assert!(uc(store, "{}").execute().await.expect("run").is_none());
    }
}
