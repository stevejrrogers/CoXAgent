//! `MockEngine` — a scripted engine for testing orchestrator/use-case logic
//! without any LLM. This is what makes the whole loop testable in CI.
//!
//! A test double: lock poisoning can only happen if a test already panicked, so
//! `expect` on the mutex is acceptable here.
#![allow(clippy::expect_used)]

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::PortError;
use std::collections::VecDeque;
use std::sync::Mutex;

/// Returns canned outputs in order; records the requests it received.
#[derive(Default)]
pub struct MockEngine {
    responses: Mutex<VecDeque<AgentOutcome>>,
    requests: Mutex<Vec<AgentRequest>>,
}

impl MockEngine {
    /// Build a mock that returns each `stdout` in sequence with exit code 0.
    #[must_use]
    pub fn with_stdouts(stdouts: impl IntoIterator<Item = String>) -> Self {
        let responses = stdouts
            .into_iter()
            .map(|stdout| AgentOutcome {
                stdout,
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: coxagent_application::ports::outbound::SandboxStatus::NotRequested,
                engine: "mock".to_owned(),
                model: String::new(),
                attempts: Vec::new(),
            })
            .collect();
        Self {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// How many times the engine was invoked.
    ///
    /// # Panics
    /// If the internal lock is poisoned (only under a prior test panic).
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.requests.lock().expect("lock").len()
    }
}

#[async_trait]
impl AgentEnginePort for MockEngine {
    fn id(&self) -> &'static str {
        "mock"
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        self.requests.lock().expect("lock").push(request);
        self.responses
            .lock()
            .expect("lock")
            .pop_front()
            .ok_or_else(|| PortError::Backend("MockEngine: no scripted response left".to_owned()))
    }
}
