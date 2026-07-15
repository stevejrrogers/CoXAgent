//! `ClaudeEngine` — runs the `claude` CLI (Claude Code) in headless print mode
//! as the agent engine. The second engine behind `AgentEnginePort`, proving the
//! Strategy boundary: swapping opencode for claude touches no use case.
//!
//! Invocation: `claude -p <prompt> --model <model> --dangerously-skip-permissions`
//! with the working directory set to the managed codebase.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest, Usage};
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
            // JSON output carries token/cost usage for FinOps tracking.
            .arg("--output-format")
            .arg("json")
            .arg("--dangerously-skip-permissions")
            .current_dir(&request.work_dir)
            // No stdin: claude -p otherwise waits for piped input and warns/exits.
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        crate::engine::apply_shim_path(&mut cmd);

        let output = tokio::time::timeout(request.timeout, cmd.output())
            .await
            .map_err(|_| PortError::Backend("claude timed out".to_owned()))?
            .map_err(|e| PortError::Backend(format!("spawn claude: {e}")))?;

        let raw = String::from_utf8_lossy(&output.stdout).into_owned();
        let (stdout, usage) = parse_json_output(&raw);

        Ok(AgentOutcome {
            stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
            usage,
        })
    }
}

/// Parse `claude --output-format json`: `{ result, total_cost_usd, usage:
/// {input_tokens, output_tokens} }`. Falls back to the raw text if it isn't the
/// expected JSON (e.g. an error line).
fn parse_json_output(raw: &str) -> (String, Option<Usage>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw.trim()) else {
        return (raw.to_owned(), None);
    };
    let text = v
        .get("result")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(raw)
        .to_owned();
    let usage = v.get("usage").map(|u| Usage {
        input_tokens: u
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        output_tokens: u
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cost_usd: v
            .get("total_cost_usd")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
    });
    (text, usage)
}
