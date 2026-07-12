//! `ClaudeEngine` — runs the `claude` CLI (Claude Code) in headless print mode
//! as the agent engine. The second engine behind `AgentEnginePort`, proving the
//! Strategy boundary: swapping opencode for claude touches no use case.
//!
//! Invocation: `claude -p <prompt> --model <model> --dangerously-skip-permissions`
//! with the working directory set to the managed codebase.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::PortError;
use tokio::process::Command;

/// Adapter over the `claude` binary for one model selection.
pub struct ClaudeEngine {
    /// Model alias or full name (e.g. `sonnet`, `opus`, `claude-sonnet-4-6`).
    model: String,
    binary: String,
}

impl ClaudeEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            binary: "claude".to_owned(),
        }
    }

    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }
}

#[async_trait]
impl AgentEnginePort for ClaudeEngine {
    fn id(&self) -> &'static str {
        "claude"
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let prompt = format!(
            "{}\n\n---\n\n{}",
            request.system_prompt, request.task_prompt
        );

        let mut cmd = Command::new(&self.binary);
        cmd.arg("-p")
            .arg(prompt)
            .arg("--model")
            .arg(&self.model)
            .arg("--output-format")
            .arg("text")
            .arg("--dangerously-skip-permissions")
            .current_dir(&request.work_dir)
            // No stdin: claude -p otherwise waits for piped input and warns/exits.
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);

        let output = tokio::time::timeout(request.timeout, cmd.output())
            .await
            .map_err(|_| PortError::Backend("claude timed out".to_owned()))?
            .map_err(|e| PortError::Backend(format!("spawn claude: {e}")))?;

        Ok(AgentOutcome {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
        })
    }
}
