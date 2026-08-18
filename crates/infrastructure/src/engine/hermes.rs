use async_trait::async_trait;
use coxagent_application::ports::outbound::engine::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus, Usage,
};
use coxagent_application::PortError;

/// Adapter over the `hermes` binary for one model selection.
pub struct HermesEngine {
    model: String,
    binary: String,
    /// Confine file writes to the project workspace (see `proc::agent_command`).
    sandbox: bool,
}

impl HermesEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            binary: crate::engine::resolve_engine_binary("hermes"),
            sandbox: false,
        }
    }

    /// Confine agent file writes to the workspace + tool caches (macOS/Linux).
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: bool) -> Self {
        self.sandbox = sandbox;
        self
    }
}

#[async_trait]
impl AgentEnginePort for HermesEngine {
    fn id(&self) -> &'static str {
        "hermes"
    }

    fn sandbox_status(&self) -> SandboxStatus {
        crate::proc::sandbox_status(self.sandbox)
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let prompt = format!(
            "{}\n\n---\n\n{}",
            request.system_prompt, request.task_prompt
        );

        let prompt_len = prompt.len();

        // nice(+10) + optional write-confinement (see proc::agent_command) —
        // same mechanism claude/opencode use, so a sandboxed run of hermes is
        // actually confined instead of spawning raw and unconfined.
        let (mut cmd, sandbox) =
            crate::proc::agent_command(&self.binary, &request.work_dir, self.sandbox);
        cmd.arg("--model")
            .arg(&self.model)
            .arg("--prompt")
            .arg(prompt)
            .current_dir(&request.work_dir)
            .stdin(std::process::Stdio::null());
        // The status comes BACK from the spawn: on a host whose Seatbelt
        // refuses to apply the profile, `sandbox` downgrades to `Denied` and
        // the outcome says the run never ran confined rather than claiming a
        // confinement that never took effect (COX-B016).
        let (output, sandbox) = crate::proc::output_confined(&mut cmd, sandbox)
            .await
            .map_err(|e| PortError::Backend(format!("spawn hermes: {e}")))?;

        // ~3.8 chars per token, as integer ceil-div of `len * 10` by 38 — same
        // result as the float form, with no lossy casts to lint around.
        let estimate = |len: usize| -> u64 {
            let chars = u64::try_from(len).unwrap_or(u64::MAX);
            chars.saturating_mul(10).saturating_add(37) / 38
        };

        Ok(AgentOutcome {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
            usage: Some(Usage {
                input_tokens: estimate(prompt_len),
                output_tokens: estimate(output.stdout.len()),
                cost_usd: 0.0,
            }),
            trace: String::new(),
            session_id: None,
            sandbox,
            engine: "hermes".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_status_defaults_to_not_requested() {
        let engine = HermesEngine::new("hermes-3-llama-3.2-3b");
        assert_eq!(engine.sandbox_status(), SandboxStatus::NotRequested);
    }

    #[test]
    fn with_sandbox_reports_confinement_or_unavailable_never_not_requested() {
        let engine = HermesEngine::new("hermes-3-llama-3.2-3b").with_sandbox(true);
        assert_ne!(engine.sandbox_status(), SandboxStatus::NotRequested);
    }
}
