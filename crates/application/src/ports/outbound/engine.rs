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

/// Token/cost usage reported by an engine, when it exposes it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

/// The raw result of an engine run. Parsing into domain effects is the caller's
/// job — the engine layer stays dumb.
#[derive(Debug, Clone, Default)]
pub struct AgentOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub usage: Option<Usage>,
    /// A human-readable step-by-step work log of the run (tool calls, edits,
    /// reasoning) when the engine can capture it — surfaced in the transcript so
    /// you can see exactly what the agent did, like a coding-agent terminal.
    /// Empty when the engine only returns a final result.
    #[doc(hidden)]
    pub trace: String,
    /// Engine-native conversation id, when the engine exposes one. Lets a
    /// follow-up run continue this conversation (see
    /// [`AgentEnginePort::resume_run`]) instead of starting cold.
    pub session_id: Option<String>,
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

    /// Continue a previous run's conversation (identified by the
    /// `session_id` that run returned) with a follow-up prompt — the agent
    /// keeps everything it just read and did in context instead of starting
    /// cold. Engines without session support keep this default.
    ///
    /// # Errors
    /// [`PortError::Backend`] when the engine has no session support, on
    /// spawn failure, or on timeout.
    async fn resume_run(
        &self,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: Duration,
    ) -> Result<AgentOutcome, PortError> {
        let _ = (session_id, follow_up, work_dir, timeout);
        Err(PortError::Backend(format!(
            "engine {} does not support session resume",
            self.id()
        )))
    }
}
