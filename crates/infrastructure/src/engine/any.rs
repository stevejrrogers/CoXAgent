//! `AnyEngine` — a static-dispatch enum over the concrete engine adapters, so
//! the composition root can pick an engine from config without boxing a trait
//! object. Add a variant here when a new engine adapter lands.

use crate::engine::{ClaudeEngine, OpencodeEngine, ScriptedEngine};
use async_trait::async_trait;
use coxagent_application::config::{EngineChoice, EngineKind};
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::PortError;

/// One of the supported engines, selected at startup.
pub enum AnyEngine {
    Opencode(OpencodeEngine),
    Claude(ClaudeEngine),
    Scripted(ScriptedEngine),
}

impl AnyEngine {
    /// Build the engine an [`EngineChoice`] names, or an error for engines that
    /// have no adapter yet.
    ///
    /// # Errors
    /// [`PortError::Backend`] when the chosen engine is not yet implemented.
    pub fn from_choice(choice: &EngineChoice) -> Result<Self, PortError> {
        match choice.engine {
            EngineKind::Opencode => Ok(Self::Opencode(OpencodeEngine::new(choice.model.clone()))),
            EngineKind::Claude => Ok(Self::Claude(ClaudeEngine::new(choice.model.clone()))),
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
            AnyEngine::Scripted(e) => e.id(),
        }
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        match self {
            AnyEngine::Opencode(e) => e.run(request).await,
            AnyEngine::Claude(e) => e.run(request).await,
            AnyEngine::Scripted(e) => e.run(request).await,
        }
    }
}
