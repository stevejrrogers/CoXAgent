//! `MeteringEngine` — a decorator over any `AgentEnginePort` that records token
//! and cost usage into a shared accumulator, attributed per agent role. This is
//! the enforce-by-architecture answer to AI FinOps: cost tracking is a wrapper,
//! so no use case knows or cares about it.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus,
};
use coxagent_application::state::Spend;
use coxagent_application::PortError;
use std::sync::{Arc, Mutex};

/// Shared, drainable spend accumulator (deltas since the last drain).
pub type Meter = Arc<Mutex<Spend>>;

/// Wraps an engine and meters its usage.
pub struct MeteringEngine<E: AgentEnginePort> {
    inner: E,
    meter: Meter,
}

impl<E: AgentEnginePort> MeteringEngine<E> {
    pub fn new(inner: E, meter: Meter) -> Self {
        Self { inner, meter }
    }
}

#[async_trait]
impl<E: AgentEnginePort> AgentEnginePort for MeteringEngine<E> {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn sandbox_status(&self) -> SandboxStatus {
        self.inner.sandbox_status()
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let role = role_key(request.role);
        // Measure the prompt we SEND per role (chars ≈ tokens/3.5): prompt
        // trimming without this number is guesswork — it names which role's
        // briefing blocks are actually fat before anyone cuts one.
        let prompt_chars = (request.system_prompt.len() + request.task_prompt.len()) as u64;
        if let Ok(mut m) = self.meter.lock() {
            *m.prompt_chars_by_role.entry(role.clone()).or_default() += prompt_chars;
        }
        let outcome = self.inner.run(request).await?;
        // Record which engine actually ran this role (stamped by FailoverEngine),
        // even when usage is unknown — the dashboard shows the live engine per
        // agent regardless of whether cost came back.
        if !outcome.engine.is_empty() {
            if let Ok(mut m) = self.meter.lock() {
                m.engine_by_role
                    .insert(role.clone(), outcome.engine.clone());
            }
        }
        if let Some(u) = outcome.usage {
            if let Ok(mut m) = self.meter.lock() {
                m.total_cost_usd += u.cost_usd;
                m.input_tokens += u.input_tokens;
                m.output_tokens += u.output_tokens;
                m.runs += 1;
                *m.by_role.entry(role.clone()).or_default() += u.cost_usd;
                *m.metered_cost_by_role.entry(role.clone()).or_default() += u.cost_usd;
                *m.runs_by_role.entry(role).or_default() += 1;
            }
        }
        self.record_sandbox(outcome.sandbox);
        Ok(outcome)
    }

    async fn resume_run(
        &self,
        role: coxagent_domain::Role,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<AgentOutcome, PortError> {
        let outcome = self
            .inner
            .resume_run(role, session_id, follow_up, work_dir, timeout)
            .await?;
        if let Some(u) = outcome.usage {
            if let Ok(mut m) = self.meter.lock() {
                m.total_cost_usd += u.cost_usd;
                m.input_tokens += u.input_tokens;
                m.output_tokens += u.output_tokens;
                m.runs += 1;
                *m.by_role.entry("resume".to_owned()).or_default() += u.cost_usd;
                *m.metered_cost_by_role
                    .entry("resume".to_owned())
                    .or_default() += u.cost_usd;
                *m.runs_by_role.entry("resume".to_owned()).or_default() += 1;
            }
        }
        self.record_sandbox(outcome.sandbox);
        Ok(outcome)
    }
}

impl<E: AgentEnginePort> MeteringEngine<E> {
    /// Roll one run's write-confinement into the shared `Spend` counters, so
    /// the dashboard can show how many runs were actually confined vs. run
    /// unconfined because `workflow.sandbox` was on but unsupported here.
    fn record_sandbox(&self, sandbox: SandboxStatus) {
        let Ok(mut m) = self.meter.lock() else {
            return;
        };
        match sandbox {
            SandboxStatus::NotRequested => {}
            SandboxStatus::Confined(via) => {
                m.confined_runs += 1;
                m.last_sandbox_status = format!("confined via {via}");
            }
            SandboxStatus::Unavailable(reason) => {
                m.unconfined_requested_runs += 1;
                m.last_sandbox_status = format!("unavailable: {reason}");
            }
            // Neither confined nor unconfined: the mechanism refused to apply
            // and the agent never ran at all (COX-B016). Counted apart so a
            // host stuck refusing Seatbelt is visible as an OS problem instead
            // of hiding inside the agent's own failure rate.
            SandboxStatus::Denied(via) => {
                m.sandbox_denied_runs += 1;
                m.last_sandbox_status =
                    format!("denied by {via}: the profile never applied, the run never started");
            }
        }
    }
}

fn role_key(role: coxagent_domain::Role) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;
    use coxagent_application::ports::outbound::Usage;
    use coxagent_domain::Role;
    use std::path::PathBuf;
    use std::time::Duration;

    struct Priced(f64);
    #[async_trait]
    impl AgentEnginePort for Priced {
        fn id(&self) -> &'static str {
            "priced"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: Some(Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cost_usd: self.0,
                }),
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::NotRequested,
                engine: "priced".to_owned(),
            })
        }
    }

    fn req(role: Role) -> AgentRequest {
        AgentRequest {
            role,
            system_prompt: String::new(),
            task_prompt: String::new(),
            work_dir: PathBuf::from("/tmp"),
            timeout: Duration::from_secs(1),
            escalation_level: 0,
            label: None,
        }
    }

    #[tokio::test]
    async fn meters_cost_and_tokens_per_role() {
        let meter: Meter = Arc::new(Mutex::new(Spend::default()));
        let eng = MeteringEngine::new(Priced(0.10), Arc::clone(&meter));
        eng.run(req(Role::DevFeature)).await.unwrap();
        eng.run(req(Role::DevFeature)).await.unwrap();
        eng.run(req(Role::Docs)).await.unwrap();

        let m = meter.lock().unwrap();
        assert_eq!(m.runs, 3);
        assert_eq!(m.input_tokens, 300);
        assert!((m.total_cost_usd - 0.30).abs() < 1e-9);
        assert!((m.by_role["dev_feature"] - 0.20).abs() < 1e-9);
        assert!((m.by_role["docs"] - 0.10).abs() < 1e-9);
    }

    /// One engine that always reports the confinement it is told to.
    struct Sandboxed(SandboxStatus);
    #[async_trait]
    impl AgentEnginePort for Sandboxed {
        fn id(&self) -> &'static str {
            "sandboxed"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                sandbox: self.0,
                exit_code: Some(71),
                ..AgentOutcome::default()
            })
        }
    }

    /// COX-B016: a run the OS refused to sandbox is neither a confined run nor
    /// an unconfined one — counting it as either hides an OS fault inside the
    /// numbers operators use to check that confinement is working.
    #[tokio::test]
    async fn a_denied_sandbox_is_metered_apart_from_confined_and_unconfined_runs() {
        let meter: Meter = Arc::new(Mutex::new(Spend::default()));
        let denied = Sandboxed(SandboxStatus::Denied("seatbelt"));
        let eng = MeteringEngine::new(denied, Arc::clone(&meter));
        eng.run(req(Role::DevBug)).await.unwrap();

        let m = meter.lock().unwrap();
        assert_eq!(m.sandbox_denied_runs, 1);
        assert_eq!(m.confined_runs, 0, "nothing ran confined");
        assert_eq!(m.unconfined_requested_runs, 0, "nothing ran at all");
        assert!(
            m.last_sandbox_status.contains("denied by seatbelt"),
            "{}",
            m.last_sandbox_status
        );
    }
}
