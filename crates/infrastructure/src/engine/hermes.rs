use async_trait::async_trait;
use coxagent_application::ports::outbound::engine::{
    AgentEnginePort, AgentOutcome, AgentRequest, Usage,
};
use coxagent_application::PortError;

/// Adapter over the `hermes` binary for one model selection.
pub struct HermesEngine {
    model: String,
    binary: String,
}

impl HermesEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            binary: "hermes".to_owned(),
        }
    }
}

#[async_trait]
impl AgentEnginePort for HermesEngine {
    fn id(&self) -> &'static str {
        "hermes"
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let prompt = format!(
            "{}\n\n---\n\n{}",
            request.system_prompt, request.task_prompt
        );

        let prompt_len = prompt.len();
        let binary = self.binary.clone();
        let model = self.model.clone();
        let work_dir = request.work_dir.clone();

        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&binary)
                .arg("--model")
                .arg(&model)
                .arg("--prompt")
                .arg(prompt)
                .current_dir(&work_dir)
                .stdin(std::process::Stdio::null())
                .output()
        })
        .await
        .map_err(|e| PortError::Backend(format!("hermes join error: {e}")))?
        .map_err(|e| PortError::Backend(format!("spawn hermes: {e}")))?;

        let estimate = |len: usize| -> u64 {
            if len == 0 {
                return 0;
            }
            (len as f64 / 3.8).ceil() as u64
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
        })
    }
}
