//! `FailoverEngine` — tries a primary engine, then falls back to the next when
//! the failure looks like a quota / rate-limit / auth wall, so a run keeps going
//! on another CLI that still has budget instead of failing until the limit
//! resets. Non-quota failures are returned as-is (another engine would fail too).

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
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
        "authentication",
        "api key",
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

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let last_idx = self.engines.len().saturating_sub(1);
        let mut last_err = String::new();
        for (i, engine) in self.engines.iter().enumerate() {
            let is_last = i == last_idx;
            match engine.run(request.clone()).await {
                // Success — done.
                Ok(o) if o.succeeded() => return Ok(o),
                // Failed: only fall through on a quota wall, and only if another
                // engine is left. A normal task failure is returned as-is.
                Ok(o) => {
                    if !is_last && is_quota_wall(&o.stderr) {
                        tracing::warn!("engine {} quota-blocked, failing over", engine.id());
                        last_err.clone_from(&o.stderr);
                        continue;
                    }
                    return Ok(o);
                }
                Err(e) => {
                    let msg = e.to_string();
                    if !is_last && is_quota_wall(&msg) {
                        tracing::warn!("engine {} error (quota), failing over", engine.id());
                        last_err = msg;
                        continue;
                    }
                    // Last engine also quota-blocked → tag it so the loop pauses.
                    if is_quota_wall(&msg) {
                        return Err(PortError::Backend(format!("{ALL_EXHAUSTED}: {msg}")));
                    }
                    return Err(e);
                }
            }
        }
        Err(PortError::Backend(format!("{ALL_EXHAUSTED}: {last_err}")))
    }
}
