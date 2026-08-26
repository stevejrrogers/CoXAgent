//! `RoutingEngine` — dispatches each agent run to a per-role engine stack.
//!
//! Not every role needs the same model. Ceremonies (standup, planning, grooming,
//! discussion — all run as `Role::Sm`) are cheap chatter that a small, fast model
//! handles just as well, while the heavy lifting (SA design, DEV code) wants the
//! strong model. Pointing the talk-heavy roles at a cheaper model is a large token
//! saving with no quality loss.
//!
//! Each route is its own full engine stack (typically a [`FailoverEngine`]), so
//! quota failover still works independently per tier. Roles with no override fall
//! through to the default stack.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus,
};
use coxagent_application::PortError;
use coxagent_domain::Role;
use std::collections::HashMap;

/// Routes runs to a per-role engine, defaulting when a role has no override.
pub struct RoutingEngine<E: AgentEnginePort> {
    default: E,
    per_role: HashMap<Role, E>,
}

impl<E: AgentEnginePort> RoutingEngine<E> {
    /// Build from the default stack and a role→stack override map.
    #[must_use]
    pub fn new(default: E, per_role: HashMap<Role, E>) -> Self {
        Self { default, per_role }
    }
}

#[async_trait]
impl<E: AgentEnginePort> AgentEnginePort for RoutingEngine<E> {
    fn id(&self) -> &'static str {
        self.default.id()
    }

    /// The default stack's confinement — sandboxing is a host-wide property,
    /// not per-role, so every route shares the same status in practice.
    fn sandbox_status(&self) -> SandboxStatus {
        self.default.sandbox_status()
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let engine = self.per_role.get(&request.role).unwrap_or(&self.default);
        engine.run(request).await
    }

    /// Route the resume to the SAME per-role engine that ran (and so minted the
    /// session id). Without this the trait default fired and every resume — the
    /// two-phase plan/execute pass, the repair pass — silently fell back to a
    /// cold run, defeating the whole point of keeping the session.
    async fn resume_run(
        &self,
        role: Role,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<AgentOutcome, PortError> {
        let engine = self.per_role.get(&role).unwrap_or(&self.default);
        engine
            .resume_run(role, session_id, follow_up, work_dir, timeout)
            .await
    }
}
