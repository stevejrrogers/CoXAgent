//! `TranscriptEngine` — a decorator that writes each agent run's prompt and
//! output to a transcript file for audit and debugging. Another enforce-by-
//! architecture concern kept out of the use cases.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::PortError;
use std::path::PathBuf;

/// Wraps an engine and persists a transcript of every run under `dir`.
pub struct TranscriptEngine<E: AgentEnginePort> {
    inner: E,
    dir: PathBuf,
}

impl<E: AgentEnginePort> TranscriptEngine<E> {
    pub fn new(inner: E, dir: PathBuf) -> Self {
        Self { inner, dir }
    }
}

#[async_trait]
impl<E: AgentEnginePort> AgentEnginePort for TranscriptEngine<E> {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let role = role_key(request.role);
        let system = request.system_prompt.clone();
        let task = request.task_prompt.clone();
        let outcome = self.inner.run(request).await;
        if let Ok(o) = &outcome {
            self.write(&role, &system, &task, o);
        }
        outcome
    }

    async fn resume_run(
        &self,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<AgentOutcome, PortError> {
        let outcome = self
            .inner
            .resume_run(session_id, follow_up, work_dir, timeout)
            .await;
        if let Ok(o) = &outcome {
            self.write(
                "resume",
                &format!("(resumed session {session_id})"),
                follow_up,
                o,
            );
        }
        outcome
    }
}

impl<E: AgentEnginePort> TranscriptEngine<E> {
    fn write(&self, role: &str, system: &str, task: &str, outcome: &AgentOutcome) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let ts = time::OffsetDateTime::now_utc().unix_timestamp();
        let path = self.dir.join(format!("{ts}-{role}.md"));
        let steps = if outcome.trace.trim().is_empty() {
            String::new()
        } else {
            format!(
                "## Work log (what the agent did)\n{}\n\n",
                outcome.trace.trim()
            )
        };
        let body = format!(
            "# {role} @ {ts}\n\n{steps}## System\n{system}\n\n## Task\n{task}\n\n## Output (exit {:?})\n{}\n\n{}",
            outcome.exit_code,
            outcome.stdout.trim(),
            if outcome.stderr.trim().is_empty() {
                String::new()
            } else {
                format!("## Stderr\n{}\n", outcome.stderr.trim())
            }
        );
        let _ = std::fs::write(path, body);
    }
}

fn role_key(role: coxagent_domain::Role) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}
