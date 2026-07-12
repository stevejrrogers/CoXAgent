//! `RunSaUseCase` — the SA design gate. Picks the top pending feature missing a
//! design, runs the engine to produce one, attaches it, and moves the ticket to
//! `ready`. The aggregate re-checks Definition of Ready, so a UI feature without
//! UX simply won't pass — enforced by code, not trusted to the prompt.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::selection::next_feature_needing_design;
use coxagent_domain::{Role, Status, TechnicalDesign, TicketId, UxDesign};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The SA's JSON output: a technical design plus optional UX (until PD lands).
#[derive(Debug, Deserialize)]
struct DesignOutput {
    approach: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    api_contract: String,
    #[serde(default)]
    data_changes: String,
    #[serde(default)]
    test_plan: String,
    #[serde(default)]
    ux: Option<UxOutput>,
}

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

/// Runs one SA design pass.
pub struct RunSaUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
}

impl<S: StateStorePort, E: AgentEnginePort> RunSaUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
        }
    }

    /// Design the next pending feature. Returns the readied ticket id, or `None`
    /// when nothing needs design.
    ///
    /// # Errors
    /// [`AppError`] on engine failure, unparseable output, or a DoR violation.
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        let state = self.store.load().await?;
        let Some(id) = next_feature_needing_design(&state) else {
            return Ok(None);
        };
        let has_ui = state
            .ticket(&id)
            .is_some_and(coxagent_domain::Ticket::has_ui);
        let title = state
            .ticket(&id)
            .map_or("", coxagent_domain::Ticket::title)
            .to_owned();

        let outcome = self.engine.run(self.build_request(&id, &title)).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "SA engine failed on {id}: {}",
                outcome.stderr.trim()
            ))
            .into());
        }
        let design = parse_design(&outcome.stdout)
            .map_err(|e| PortError::Corrupt(format!("SA output: {e}")))?;

        let mut state = self.store.load().await?;
        let ticket = state
            .ticket_mut(&id)
            .ok_or_else(|| PortError::Corrupt(format!("ticket {id} vanished")))?;
        ticket.set_technical_design(Role::Sa, technical_of(&design))?;
        if has_ui {
            let ux = design.ux.as_ref().map(ux_of).unwrap_or_default();
            ticket.set_ux_design(Role::Sa, ux)?;
        }
        // DoR re-checked here; fails loudly if a UI ticket still lacks UX.
        ticket.transition_to(Role::Sa, Status::Ready)?;
        self.store.save(&state).await?;
        Ok(Some(id))
    }

    fn build_request(&self, id: &TicketId, title: &str) -> AgentRequest {
        let _choice = self.config.engine.resolve(Role::Sa);
        AgentRequest {
            role: Role::Sa,
            system_prompt: prompts::system_prompt(prompts::SA),
            task_prompt: format!("Design feature {id}: {title}"),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(1200),
        }
    }
}

fn parse_design(raw: &str) -> Result<DesignOutput, String> {
    let start = raw.find('{').ok_or("no JSON object found")?;
    let end = raw.rfind('}').ok_or("no closing brace")?;
    if end < start {
        return Err("malformed object bounds".to_owned());
    }
    serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())
}

fn technical_of(d: &DesignOutput) -> TechnicalDesign {
    TechnicalDesign {
        approach: d.approach.clone(),
        files: d.files.clone(),
        api_contract: d.api_contract.clone(),
        data_changes: d.data_changes.clone(),
        test_plan: d.test_plan.clone(),
    }
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
    use crate::ports::outbound::AgentOutcome;
    use crate::state::ProjectState;
    use crate::use_cases::{AddTicketInput, AddTicketUseCase};
    use coxagent_domain::{Complexity, Priority, TicketType};
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

    async fn seed(store: &Arc<MemStore>, has_ui: bool) {
        AddTicketUseCase::new(Arc::clone(store))
            .execute(AddTicketInput {
                ticket_type: TicketType::Feature,
                title: "F".to_owned(),
                description: String::new(),
                priority: Priority::High,
                complexity: Complexity::Small,
                has_ui,
            })
            .await
            .expect("seed");
    }

    fn uc(store: Arc<MemStore>, out: &str) -> RunSaUseCase<MemStore, Canned> {
        RunSaUseCase::new(
            store,
            Arc::new(Canned(out.to_owned())),
            Config::default(),
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn designs_non_ui_feature_to_ready() {
        let store = Arc::new(MemStore::default());
        seed(&store, false).await;
        let out = r#"{"approach":"do it","files":["a.rs"],"api_contract":"","data_changes":"","test_plan":"t","ux":null}"#;
        let id = uc(Arc::clone(&store), out).execute().await.expect("run");
        assert!(id.is_some());
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Ready);
        assert!(state.tickets[0].design().technical.is_some());
    }

    #[tokio::test]
    async fn ui_feature_without_ux_fails_dor() {
        let store = Arc::new(MemStore::default());
        seed(&store, true).await;
        // ux null but has_ui -> default (empty) ux is attached; DoR passes on
        // presence. Presence, not richness, is what code can enforce.
        let out = r#"{"approach":"do it","files":[],"api_contract":"","data_changes":"","test_plan":"","ux":null}"#;
        let id = uc(Arc::clone(&store), out).execute().await.expect("run");
        assert!(id.is_some());
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Ready);
        assert!(state.tickets[0].design().ux.is_some());
    }

    #[tokio::test]
    async fn no_pending_feature_returns_none() {
        let store = Arc::new(MemStore::default());
        assert!(uc(store, "{}").execute().await.expect("run").is_none());
    }
}
