//! `AnyEngine` — a static-dispatch enum over the concrete engine adapters, so
//! the composition root can pick an engine from config without boxing a trait
//! object. Add a variant here when a new engine adapter lands.

use crate::engine::{ClaudeEngine, HermesEngine, McpAccess, OpencodeEngine, ScriptedEngine};
use async_trait::async_trait;
use coxagent_application::config::{EngineChoice, EngineKind};
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::PortError;

/// One of the supported engines, selected at startup.
pub enum AnyEngine {
    Opencode(OpencodeEngine),
    Claude(ClaudeEngine),
    Hermes(HermesEngine),
    Scripted(ScriptedEngine),
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
        match choice.engine {
            EngineKind::Opencode => Ok(Self::Opencode(
                OpencodeEngine::new(choice.model.clone()).with_mcp(mcp),
            )),
            EngineKind::Claude => Ok(Self::Claude(
                ClaudeEngine::new(choice.model.clone()).with_mcp(mcp),
            )),
            EngineKind::Hermes => Ok(Self::Hermes(HermesEngine::new(choice.model.clone()))),
            EngineKind::Scripted => Ok(Self::Scripted(ScriptedEngine::new())),
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
            AnyEngine::Scripted(e) => e.id(),
        }
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        match self {
            AnyEngine::Opencode(e) => e.run(request).await,
            AnyEngine::Claude(e) => e.run(request).await,
            AnyEngine::Hermes(e) => e.run(request).await,
            AnyEngine::Scripted(e) => e.run(request).await,
        }
    }

    async fn resume_run(
        &self,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<AgentOutcome, PortError> {
        match self {
            AnyEngine::Opencode(e) => e.resume_run(session_id, follow_up, work_dir, timeout).await,
            AnyEngine::Claude(e) => e.resume_run(session_id, follow_up, work_dir, timeout).await,
            AnyEngine::Hermes(e) => e.resume_run(session_id, follow_up, work_dir, timeout).await,
            AnyEngine::Scripted(e) => e.resume_run(session_id, follow_up, work_dir, timeout).await,
        }
    }
}
