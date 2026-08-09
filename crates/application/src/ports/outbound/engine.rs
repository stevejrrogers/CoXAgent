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
    /// Retry escalation level: 0 = first attempt (engine's configured model);
    /// 1+ = the ticket already failed that many times, so the engine should
    /// run a STRONGER model from its escalation ladder. Engines without a
    /// ladder ignore it.
    pub escalation_level: u8,
    /// Optional human-readable tag naming what this run is for (typically a
    /// TicketId like "CXA-F004"). Threaded into harness session/live-log naming
    /// so runs are chaseable per ticket.
    pub label: Option<String>,
}

/// Token/cost usage reported by an engine, when it exposes it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

/// Whether — and how — an engine confined the agent's file writes for one run.
/// Owned by the application layer (this port) even though only infrastructure
/// can determine it, because it rides on [`AgentOutcome`], a port type: infra
/// constructs values of this enum, keeping the dependency direction inward.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SandboxStatus {
    /// `workflow.sandbox` was off for this run — no confinement was attempted.
    #[default]
    NotRequested,
    /// Writes were confined to the workspace/tool-cache allowlist by the named
    /// mechanism (e.g. `"seatbelt"`, `"bwrap"`).
    Confined(&'static str),
    /// `workflow.sandbox` was on but this host has no supported confinement
    /// mechanism — the run still executed unconfined. The payload is a short
    /// human-readable reason.
    Unavailable(&'static str),
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
    /// Write confinement actually applied to this run (see [`SandboxStatus`]).
    pub sandbox: SandboxStatus,
    /// The engine CLI that ACTUALLY produced this outcome (`claude`, `opencode`,
    /// `copilot`, …) — stamped by [`FailoverEngine`] with the id of whichever
    /// engine won, so the dashboard shows the engine really running a role
    /// instead of the one config asked for (they differ the moment failover
    /// fires). Empty when unstamped (a bare engine or a test double).
    pub engine: String,
}

impl AgentOutcome {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// Why this run failed, for an error message. Prefers stderr, but falls
    /// back to the tail of stdout: CLI engines print their real reason (an
    /// expired OAuth session, a quota wall) as a normal output event and exit
    /// with an EMPTY stderr, which would otherwise strip the cause out of the
    /// error — and with it every downstream signal that reads the message,
    /// including the infrastructure-fault taxonomy.
    #[must_use]
    pub fn failure_detail(&self) -> String {
        let err = self.stderr.trim();
        if !err.is_empty() {
            return err.to_owned();
        }
        let out = self.stdout.trim();
        if out.is_empty() {
            return String::new();
        }
        // The last lines carry the failure; cap it so an error stays readable.
        let tail: Vec<&str> = out.lines().rev().take(6).collect();
        let mut detail: String = tail
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ")
            .chars()
            .take(400)
            .collect();
        detail = detail.trim().to_owned();
        detail
    }
}

/// A CLI agent engine.
#[async_trait]
pub trait AgentEnginePort: Send + Sync {
    /// Stable identifier for logging/telemetry (e.g. `"opencode"`).
    fn id(&self) -> &'static str;

    /// The write-confinement this engine would apply right now, independent of
    /// any specific request — a function of its own `sandbox` setting and the
    /// host platform's confinement support. Engines without sandboxing (or
    /// decorators that forget to forward) keep the default `NotRequested`, so
    /// this must be overridden by every real engine AND every decorator that
    /// wraps one, or the platform-support warning silently never fires.
    fn sandbox_status(&self) -> SandboxStatus {
        SandboxStatus::NotRequested
    }

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
    /// `role` names the agent whose session this is, so a routing engine can
    /// resume on the SAME per-role engine that minted the session id — a session
    /// id is engine-native, so resuming it anywhere else just fails to a cold run.
    async fn resume_run(
        &self,
        role: Role,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: Duration,
    ) -> Result<AgentOutcome, PortError> {
        let _ = (role, session_id, follow_up, work_dir, timeout);
        Err(PortError::Backend(format!(
            "engine {} does not support session resume",
            self.id()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(stdout: &str, stderr: &str) -> AgentOutcome {
        AgentOutcome {
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            exit_code: Some(1),
            usage: None,
            trace: String::new(),
            session_id: None,
            sandbox: SandboxStatus::default(),
        }
    }

    #[test]
    fn failure_detail_falls_back_to_stdout_when_stderr_is_empty() {
        // The claude CLI reports an expired session on stdout and exits with an
        // empty stderr; dropping it strips the cause out of every downstream
        // signal, including the infra-fault taxonomy that pauses the runner.
        let o = outcome(
            "starting\nFailed to authenticate: OAuth session expired and could not be refreshed",
            "",
        );
        let detail = o.failure_detail();
        assert!(
            detail.contains("OAuth session expired"),
            "detail was {detail:?}"
        );
        assert!(crate::faults::is_infra_fault(&detail));
    }

    #[test]
    fn failure_detail_prefers_stderr_and_tolerates_silence() {
        assert_eq!(
            outcome("noise on stdout", "  real reason  ").failure_detail(),
            "real reason"
        );
        assert_eq!(outcome("", "").failure_detail(), "");
    }

    #[test]
    fn failure_detail_is_bounded() {
        let long = "x".repeat(5000);
        assert!(outcome(&long, "").failure_detail().len() <= 400);
    }
}
