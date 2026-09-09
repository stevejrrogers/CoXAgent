//! `AnyEngine` — a static-dispatch enum over the concrete engine adapters, so
//! the composition root can pick an engine from config without boxing a trait
//! object. Add a variant here when a new engine adapter lands.

use crate::engine::{
    ClaudeEngine, CopilotEngine, HarxesEngine, HermesEngine, McpAccess, OpencodeEngine,
    ScriptedEngine,
};
use async_trait::async_trait;
use coxagent_application::config::{EngineChoice, EngineKind};
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus,
};
use coxagent_application::PortError;

/// One of the supported engines, selected at startup.
pub enum AnyEngine {
    Opencode(OpencodeEngine),
    Claude(ClaudeEngine),
    Hermes(HermesEngine),
    Copilot(CopilotEngine),
    Scripted(ScriptedEngine),
    Harxes(HarxesEngine),
}

impl AnyEngine {
    /// Build the engine an [`EngineChoice`] names, or an error for engines that
    /// have no adapter yet. `mcp`, when present, gives the tool-calling
    /// engines (claude, opencode) live access to this project's code-graph MCP
    /// endpoint — see [`McpAccess`]. Hermes has no tool-use loop so it's
    /// ignored there; those roles still get the code graph via the static
    /// repo-map/focus-block prompt text every engine receives.
    ///
    /// # Errors
    /// [`PortError::Backend`] when the chosen engine is not yet implemented.
    pub fn from_choice(choice: &EngineChoice, mcp: Option<McpAccess>) -> Result<Self, PortError> {
        Self::from_choice_with_escalation(choice, mcp, &[], false)
    }

    /// Like [`Self::from_choice`], threading the configured retry escalation
    /// ladder into engines that support it (empty = engine defaults: claude →
    /// opus; opencode → its config's custom providers first, then built-ins).
    ///
    /// # Errors
    /// [`PortError::Backend`] when the chosen engine is not yet implemented.
    pub fn from_choice_with_escalation(
        choice: &EngineChoice,
        mcp: Option<McpAccess>,
        escalation: &[String],
        sandbox: bool,
    ) -> Result<Self, PortError> {
        match choice.engine {
            EngineKind::Opencode => Ok(Self::Opencode(
                OpencodeEngine::new(choice.model.clone())
                    .with_mcp(mcp)
                    .with_escalation(escalation.to_vec())
                    .with_sandbox(sandbox),
            )),
            EngineKind::Claude => Ok(Self::Claude(
                ClaudeEngine::new(choice.model.clone())
                    .with_mcp(mcp)
                    .with_escalation(escalation.to_vec())
                    .with_sandbox(sandbox),
            )),
            EngineKind::Hermes => Ok(Self::Hermes(
                HermesEngine::new(choice.model.clone()).with_sandbox(sandbox),
            )),
            EngineKind::Copilot => Ok(Self::Copilot(
                CopilotEngine::new(choice.model.clone()).with_sandbox(sandbox),
            )),
            EngineKind::Scripted => Ok(Self::Scripted(ScriptedEngine::new())),
            EngineKind::Harxes => Ok(Self::Harxes(
                HarxesEngine::new(choice.model.clone()).with_sandbox(sandbox),
            )),
            other => Err(PortError::Backend(format!(
                "no adapter for engine {other:?} yet"
            ))),
        }
    }
}

#[async_trait]
impl AgentEnginePort for AnyEngine {
    fn id(&self) -> &'static str {
        match self {
            AnyEngine::Opencode(e) => e.id(),
            AnyEngine::Claude(e) => e.id(),
            AnyEngine::Hermes(e) => e.id(),
            AnyEngine::Copilot(e) => e.id(),
            AnyEngine::Scripted(e) => e.id(),
            AnyEngine::Harxes(e) => e.id(),
        }
    }

    fn sandbox_status(&self) -> SandboxStatus {
        match self {
            AnyEngine::Opencode(e) => e.sandbox_status(),
            AnyEngine::Claude(e) => e.sandbox_status(),
            AnyEngine::Hermes(e) => e.sandbox_status(),
            AnyEngine::Copilot(e) => e.sandbox_status(),
            AnyEngine::Scripted(e) => e.sandbox_status(),
            AnyEngine::Harxes(e) => e.sandbox_status(),
        }
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        match self {
            AnyEngine::Opencode(e) => e.run(request).await,
            AnyEngine::Claude(e) => e.run(request).await,
            AnyEngine::Hermes(e) => e.run(request).await,
            AnyEngine::Copilot(e) => e.run(request).await,
            AnyEngine::Scripted(e) => e.run(request).await,
            AnyEngine::Harxes(e) => e.run(request).await,
        }
    }

    async fn resume_run(
        &self,
        role: coxagent_domain::Role,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<AgentOutcome, PortError> {
        match self {
            AnyEngine::Opencode(e) => {
                e.resume_run(role, session_id, follow_up, work_dir, timeout)
                    .await
            }
            AnyEngine::Claude(e) => {
                e.resume_run(role, session_id, follow_up, work_dir, timeout)
                    .await
            }
            AnyEngine::Hermes(e) => {
                e.resume_run(role, session_id, follow_up, work_dir, timeout)
                    .await
            }
            AnyEngine::Copilot(e) => {
                e.resume_run(role, session_id, follow_up, work_dir, timeout)
                    .await
            }
            AnyEngine::Scripted(e) => {
                e.resume_run(role, session_id, follow_up, work_dir, timeout)
                    .await
            }
            AnyEngine::Harxes(e) => {
                e.resume_run(role, session_id, follow_up, work_dir, timeout)
                    .await
            }
        }
    }
}
