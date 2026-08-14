//! `FailoverEngine` — tries a primary engine, then falls back to the next when
//! the failure looks like a quota / rate-limit / auth wall, so a run keeps going
//! on another CLI that still has budget instead of failing until the limit
//! resets. Non-quota failures are returned as-is (another engine would fail too).

use async_trait::async_trait;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus,
};
use coxagent_application::PortError;

/// Marker prefix on the returned error/outcome when every engine is quota-blocked
/// — the cycle detects it to pause the loop rather than spin uselessly.
pub const ALL_EXHAUSTED: &str = "ALL_ENGINES_QUOTA_EXHAUSTED";

/// Whether a message looks like a quota / rate-limit / auth exhaustion (vs a
/// normal task failure we should NOT retry on another engine).
#[must_use]
pub fn is_quota_wall(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "quota",
        "rate limit",
        "rate-limit",
        "429",
        "usage limit",
        "usage-limit",
        "overloaded",
        "insufficient",
        "out of tokens",
        "credit",
        "billing",
        "too many requests",
        "resource_exhausted",
        "401",
        "403",
        "unauthorized",
        // Both noun and verb forms: the Claude CLI says "Failed to
        // authenticate", which the noun "authentication" does not cover.
        "authentication",
        "authenticate",
        // OAuth session death: "OAuth access token has been revoked" and
        // "OAuth session expired and could not be refreshed" — neither carried
        // a 401 on the direct CLI path, so failover never fired and the whole
        // team stalled on a dead engine while a live one sat idle.
        "revoked",
        "oauth",
        "session expired",
        "token expired",
        "expired token",
        "api key",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// Whether a message looks like a transient failure (a hang/timeout, a dropped
/// connection, a stalled stream) worth retrying on another engine — the model is
/// stuck or slow, not the task being wrong. Unlike a quota wall this does NOT
/// exhaust the loop: if every engine is merely slow, the next cycle retries.
#[must_use]
pub fn is_transient(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "timed out",
        "timeout",
        "deadline",
        "connection",
        "stream closed",
        "broken pipe",
        "reset by peer",
        "temporarily",
        "try again",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// Runs the first engine that isn't quota-blocked.
pub struct FailoverEngine<E: AgentEnginePort> {
    engines: Vec<E>,
}

impl<E: AgentEnginePort> FailoverEngine<E> {
    /// Build from an ordered list (primary first). Panics only via callers who
    /// pass an empty list — build with at least one engine.
    #[must_use]
    pub fn new(engines: Vec<E>) -> Self {
        Self { engines }
    }
}

#[async_trait]
impl<E: AgentEnginePort> AgentEnginePort for FailoverEngine<E> {
    fn id(&self) -> &'static str {
        self.engines.first().map_or("failover", AgentEnginePort::id)
    }

    fn sandbox_status(&self) -> SandboxStatus {
        self.engines
            .first()
            .map_or(SandboxStatus::NotRequested, AgentEnginePort::sandbox_status)
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let last_idx = self.engines.len().saturating_sub(1);
        let mut last_err = String::new();
        for (i, engine) in self.engines.iter().enumerate() {
            let is_last = i == last_idx;
            match engine.run(request.clone()).await {
                // Success — done. The concrete engine has already stamped
                // `outcome.engine` with its own id, so the outcome that returns
                // here already names the engine that actually ran (post-failover).
                Ok(o) if o.succeeded() => return Ok(o),
                // Failed: only fall through on a quota wall, and only if another
                // engine is left. A normal task failure is returned as-is.
                Ok(o) => {
                    // Fall over on a quota wall or a transient stall (slow/hung
                    // engine), as long as another engine is left to try.
                    if !is_last && (is_quota_wall(&o.stderr) || is_transient(&o.stderr)) {
                        tracing::warn!("engine {} unavailable, failing over", engine.id());
                        last_err.clone_from(&o.stderr);
                        continue;
                    }
                    return Ok(o);
                }
                Err(e) => {
                    let msg = e.to_string();
                    if !is_last && (is_quota_wall(&msg) || is_transient(&msg)) {
                        tracing::warn!("engine {} error, failing over: {msg}", engine.id());
                        last_err = msg;
                        continue;
                    }
                    // Every engine was quota-blocked → tag it so the loop pauses
                    // (a transient stall is NOT tagged: the next cycle retries).
                    if is_quota_wall(&msg) {
                        return Err(PortError::Backend(format!("{ALL_EXHAUSTED}: {msg}")));
                    }
                    return Err(e);
                }
            }
        }
        Err(PortError::Backend(format!("{ALL_EXHAUSTED}: {last_err}")))
    }

    /// A session id belongs to the engine that minted it, so resume goes to
    /// the primary only — never failed over to an engine that has no such
    /// session. Callers fall back to a full fresh run on error.
    async fn resume_run(
        &self,
        role: coxagent_domain::Role,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<AgentOutcome, PortError> {
        match self.engines.first() {
            Some(e) => {
                e.resume_run(role, session_id, follow_up, work_dir, timeout)
                    .await
            }
            None => Err(PortError::Backend("failover: no engines".to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_quota_wall, is_transient};

    #[test]
    fn detects_quota_walls() {
        assert!(is_quota_wall("Error 429: rate limit exceeded"));
        assert!(is_quota_wall("usage limit reached for your plan"));
        assert!(is_quota_wall("401 Unauthorized: invalid api key"));
        // The exact CLI strings that stalled the whole team on a dead engine
        // while a live one sat idle — none carried a 401 on the direct path.
        assert!(is_quota_wall(
            "Failed to authenticate. API Error: 401 OAuth access token has been revoked."
        ));
        assert!(is_quota_wall(
            "Failed to authenticate: OAuth session expired and could not be refreshed"
        ));
        assert!(!is_quota_wall("compile error: missing semicolon"));
    }

    #[test]
    fn detects_transient_stalls() {
        assert!(is_transient("claude timed out"));
        assert!(is_transient("connection reset by peer"));
        assert!(is_transient("stream closed unexpectedly"));
        assert!(!is_transient("the tests failed"));
    }
}
