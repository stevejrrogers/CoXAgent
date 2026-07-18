//! `RunMilestonesUseCase` — the PO lays out the product milestones once, from
//! the goal and current backlog. Milestones are the roadmap targets that sprints
//! work toward; each has a `target_version` that marks it reached. Authored a
//! single time (idempotent) like the design system.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::state::Milestone;
use coxagent_domain::Role;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct MilestoneOut {
    name: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    target_version: String,
}

/// Authors the milestone roadmap when absent.
pub struct RunMilestonesUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    context: String,
}

impl<S: StateStorePort, E: AgentEnginePort> RunMilestonesUseCase<S, E> {
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
        }
    }

    /// Author the milestone plan once, when the project has a backlog but no
    /// milestones yet. Returns `true` when a plan was created this pass.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or unparseable output.
    pub async fn execute(&self) -> Result<bool, AppError> {
        let state = self.store.load().await?;
        if !state.milestones.is_empty() || state.tickets.is_empty() {
            return Ok(false);
        }
        let _ = self.config.engine.resolve(Role::Po);

        let backlog: Vec<String> = state
            .tickets
            .iter()
            .map(|t| format!("- {}", t.title()))
            .collect();
        let request = AgentRequest {
            role: Role::Po,
            system_prompt: prompt_system(),
            task_prompt: format!(
                "Product goal:\n{}\n\nCurrent backlog:\n{}\n\nLay out 3-5 sequential product \
                 milestones from earliest to latest. Each milestone is a meaningful, shippable \
                 outcome that may take several sprints. Respond with ONLY a JSON array, each item \
                 exactly: {{\"name\": string, \"goal\": string, \"target_version\": \"MAJOR.MINOR.0\"}} \
                 with strictly increasing target_version (e.g. 0.2.0, 0.5.0, 1.0.0).",
                self.context,
                backlog.join("\n")
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(300),
        };

        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "PO milestones engine failed: {}",
                outcome.stderr.trim()
            ))
            .into());
        }
        let parsed = parse(&outcome.stdout)
            .map_err(|e| PortError::Corrupt(format!("PO milestones output: {e}")))?;
        let milestones: Vec<Milestone> = parsed
            .into_iter()
            .filter(|m| !m.name.trim().is_empty())
            .map(|m| Milestone {
                name: m.name.trim().to_owned(),
                goal: m.goal.trim().to_owned(),
                target_version: m.target_version.trim().to_owned(),
            })
            .take(6)
            .collect();
        if milestones.is_empty() {
            return Ok(false);
        }

        let mut state = self.store.load().await?;
        if !state.milestones.is_empty() {
            return Ok(false); // raced with another writer
        }
        let n = milestones.len();
        state.milestones = milestones;
        state.log_activity("PO", &format!("planned {n} milestones"), None);
        state.post_comment(
            "PO",
            &format!(
                "Roadmap set — {n} milestones from here to launch. Each one is a real \
                      shippable target; sprints ladder up to them."
            ),
            None,
        );
        self.store.save(&state).await?;
        Ok(true)
    }
}

fn prompt_system() -> String {
    crate::prompts::system_prompt(crate::prompts::PO)
}

fn parse(raw: &str) -> Result<Vec<MilestoneOut>, String> {
    let start = raw.find('[').ok_or("no JSON array found")?;
    let end = raw.rfind(']').ok_or("no closing bracket")?;
    if end < start {
        return Err("malformed array bounds".to_owned());
    }
    serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())
}
