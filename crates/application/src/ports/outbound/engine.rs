//! `AgentEnginePort` — the Strategy boundary over CLI agent engines
//! (opencode, claude, ...). Use cases depend on this trait; infrastructure
//! provides real and mock adapters. Swapping engines never touches a use case.

use crate::error::PortError;
use async_trait::async_trait;
use coxagent_domain::Role;
use std::path::PathBuf;
use std::time::Duration;

/// One invocation of an agent: a composed prompt run in a working directory.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub role: Role,
    pub system_prompt: String,
    pub task_prompt: String,
    pub work_dir: PathBuf,
    pub timeout: Duration,
}

/// The raw result of an engine run. Parsing into domain effects is the caller's
/// job — the engine layer stays dumb.
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

impl AgentOutcome {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// A CLI agent engine.
#[async_trait]
pub trait AgentEnginePort: Send + Sync {
    /// Stable identifier for logging/telemetry (e.g. `"opencode"`).
    fn id(&self) -> &'static str;

    /// Run the request to completion (or timeout) and return its output.
    ///
    /// # Errors
    /// [`PortError::Backend`] on spawn failure or timeout.
    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError>;
}
