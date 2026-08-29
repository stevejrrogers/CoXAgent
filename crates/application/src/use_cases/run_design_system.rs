//! `RunDesignSystemUseCase` — PD establishes the project-level design system
//! once, the first time the backlog contains any UI work. The result is stored
//! on the project and injected into every DEV prompt for a UI ticket, so the
//! visual language is enforced proactively (like architecture governance)
//! rather than re-invented per ticket.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::state::DesignSystem;
use coxagent_domain::Role;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct DesignSystemOutput {
    #[serde(default)]
    principles: String,
    #[serde(default)]
    palette: Vec<String>,
    #[serde(default)]
    typography: String,
    #[serde(default)]
    components: Vec<String>,
}

/// Authors the project design system when absent.
pub struct RunDesignSystemUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
}

impl<S: StateStorePort, E: AgentEnginePort> RunDesignSystemUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
        }
    }

    /// Author the design system if the project has UI work but none yet.
    /// Returns `true` when a design system was created this pass.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or unparseable output.
    pub async fn execute(&self) -> Result<bool, AppError> {
        let state = self.store.load().await?;
        let already = state
            .design_system
            .as_ref()
            .is_some_and(DesignSystem::is_populated);
        let has_ui_work = state.tickets.iter().any(coxagent_domain::Ticket::has_ui);
        if already || !has_ui_work {
            return Ok(false);
        }

        let outcome = self.engine.run(self.build_request()).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "PD design-system engine failed: {}",
                outcome.failure_detail()
            ))
            .into());
        }
        let parsed = parse(&outcome.stdout)
            .map_err(|e| PortError::Corrupt(format!("PD design-system output: {e}")))?;
        let ds = DesignSystem {
            principles: parsed.principles,
            palette: parsed.palette,
            typography: parsed.typography,
            components: parsed.components,
        };
        if !ds.is_populated() {
            return Ok(false);
        }

        let mut state = self.store.load().await?;
        state.design_system = Some(ds);
        self.store.save(&state).await?;
        Ok(true)
    }

    fn build_request(&self) -> AgentRequest {
        let _choice = self.config.engine.resolve(Role::Pd);
        AgentRequest {
            role: Role::Pd,
            system_prompt: prompts::system_prompt(prompts::DESIGN_SYSTEM),
            task_prompt: "Establish the design system for this product.".to_owned(),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(1200),
            escalation_level: 0,
            label: None,
        }
    }
}

fn parse(raw: &str) -> Result<DesignSystemOutput, String> {
    let start = raw.find('{').ok_or("no JSON object found")?;
    let end = raw.rfind('}').ok_or("no closing brace")?;
    if end < start {
        return Err("malformed object bounds".to_owned());
    }
    serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
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
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
                engine: String::new(),
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
                acceptance_criteria: Vec::new(),
                goal: None,
            })
            .await
            .expect("seed");
    }

    const OUT: &str = r#"{"principles":"calm, focused","palette":["primary: cyan #0891B2"],"typography":"Inter","components":["buttons: 8px radius"]}"#;

    fn uc(store: Arc<MemStore>, out: &str) -> RunDesignSystemUseCase<MemStore, Canned> {
        RunDesignSystemUseCase::new(
            store,
            Arc::new(Canned(out.to_owned())),
            Config::default(),
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn authors_once_when_ui_work_exists() {
        let store = Arc::new(MemStore::default());
        seed(&store, true).await;
        assert!(uc(Arc::clone(&store), OUT).execute().await.expect("run"));
        let ds = store.load().await.expect("load").design_system.expect("ds");
        assert_eq!(ds.palette, vec!["primary: cyan #0891B2".to_owned()]);
        // Idempotent: a second pass does nothing (already populated).
        assert!(!uc(Arc::clone(&store), OUT).execute().await.expect("run 2"));
    }

    #[tokio::test]
    async fn skips_when_no_ui_work() {
        let store = Arc::new(MemStore::default());
        seed(&store, false).await;
        assert!(!uc(store, OUT).execute().await.expect("run"));
    }
}
