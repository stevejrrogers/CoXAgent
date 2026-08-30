//! `FailoverEngine` — tries a primary engine, then falls back to the next when
//! the failure looks like a quota / rate-limit / auth wall, so a run keeps going
//! on another CLI that still has budget instead of failing until the limit
//! resets. Non-quota failures are returned as-is (another engine would fail too).

use async_trait::async_trait;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus,
};
use coxagent_application::state::EngineAttempt;
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
        // CXA-F257: every attempt of this logical run, in run order. Only the
        // failover layer knows the sequence, so it is stamped here — the
        // metering decorator folds the whole trail into the step's provenance
        // and the verify gate sees the primary that died, not only the engine
        // that finished the work.
        let mut attempts: Vec<EngineAttempt> = Vec::new();
        for (i, engine) in self.engines.iter().enumerate() {
            let is_last = i == last_idx;
            // A fallback engine inherits whatever the dead engine left behind:
            // uncommitted edits in the working tree and possibly a BRIEF note.
            // Without saying so, it re-plans from zero, redoing (and often
            // conflicting with) minutes of paid work. Tell it to CONTINUE.
            let mut request = request.clone();
            if i > 0 {
                request.task_prompt = format!(
                    "{}\n\n## Continuation after engine failover\n\
                     A previous engine started this exact task and died mid-run \
                     (quota/outage). Its partial work may be in the working tree \
                     as uncommitted changes, and a handoff note may exist under \
                     `.coxagent/briefs/`. FIRST inspect `git status`/`git diff` \
                     (and the brief if present), then CONTINUE from that state — \
                     keep what is correct, finish what is missing. Do not start \
                     over or revert work you did not write.",
                    request.task_prompt
                );
            }
            let result = engine.run(request).await;
            // Every completed attempt — success or failure — joins the trail
            // in order. A run that errored before producing an outcome still
            // names its engine; its model is genuinely unknown (AC3's case).
            attempts.push(match &result {
                Ok(o) => EngineAttempt {
                    engine: if o.engine.is_empty() {
                        AgentEnginePort::id(engine).to_owned()
                    } else {
                        o.engine.clone()
                    },
                    model: coxagent_application::engine_provenance::model_id(&o.model),
                },
                Err(_) => EngineAttempt {
                    engine: AgentEnginePort::id(engine).to_owned(),
                    model: None,
                },
            });
            match result {
                // Success — done. The concrete engine has already stamped
                // `outcome.engine` with its own id, so the outcome that returns
                // here already names the engine that actually ran (post-failover).
                Ok(o) if o.succeeded() => return Ok(stamp_trail(o, attempts)),
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
                    return Ok(stamp_trail(o, attempts));
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

/// Attach the run's attempt trail to the outcome leaving the chain, so the
/// metering decorator folds EVERY attempt — not only the winner — into the
/// step's provenance record (CXA-F257 AC2).
fn stamp_trail(mut o: AgentOutcome, attempts: Vec<EngineAttempt>) -> AgentOutcome {
    o.attempts = attempts;
    o
}

#[cfg(test)]
mod tests {
    use super::{is_quota_wall, is_transient, stamp_trail};
    use crate::engine::metering::MeteringEngine;
    use async_trait::async_trait;
    use coxagent_application::ports::outbound::{
        AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus,
    };
    use coxagent_application::PortError;
    use std::path::PathBuf;
    use std::time::Duration;

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

    /// A fake engine with a fixed id, outcome and success — the fixture for
    /// the attempt-trail regressions below. `fail` makes `run` error (a
    /// spawn/connection failure) instead of returning a failed outcome, so
    /// both failover triggers are exercisable with one double.
    struct Fixed {
        id: &'static str,
        engine: &'static str,
        model: &'static str,
        stderr: &'static str,
        code: i32,
        fail: bool,
    }
    #[async_trait]
    impl AgentEnginePort for Fixed {
        fn id(&self) -> &'static str {
            self.id
        }
        fn sandbox_status(&self) -> SandboxStatus {
            SandboxStatus::NotRequested
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            if self.fail {
                return Err(PortError::Backend("connection reset by peer".to_owned()));
            }
            Ok(AgentOutcome {
                stderr: self.stderr.to_owned(),
                exit_code: Some(self.code),
                engine: self.engine.to_owned(),
                model: self.model.to_owned(),
                ..AgentOutcome::default()
            })
        }
    }

    fn request() -> AgentRequest {
        AgentRequest {
            role: coxagent_domain::Role::DevBug,
            system_prompt: String::new(),
            task_prompt: String::new(),
            work_dir: PathBuf::from("/tmp"),
            timeout: Duration::from_secs(1),
            escalation_level: 0,
            label: None,
        }
    }

    /// CXA-F257 AC2: a run that failed over mid-step keeps EVERY attempt in
    /// order on the returned outcome — the quota-walled primary first, the
    /// fallback that finished the work second — not only the final engine.
    #[tokio::test]
    async fn a_failed_over_run_keeps_every_attempt_in_order() {
        let primary = Fixed {
            id: "claude",
            engine: "claude",
            model: "opus",
            stderr: "429 rate limit exceeded",
            code: 1,
            fail: false,
        };
        let fallback = Fixed {
            id: "opencode",
            engine: "opencode",
            model: "bizbrain/Qwen3.6-35B-A3B-thinking",
            stderr: "",
            code: 0,
            fail: false,
        };
        let chain = super::FailoverEngine::new(vec![primary, fallback]);
        let o = chain.run(request()).await.expect("fallback succeeds");
        assert!(o.succeeded());
        assert_eq!(o.engine, "opencode", "the winner is stamped as today");
        let attempts: Vec<(String, Option<String>)> = o
            .attempts
            .iter()
            .map(|a| (a.engine.clone(), a.model.clone()))
            .collect();
        assert_eq!(
            attempts,
            vec![
                ("claude".to_owned(), Some("opus".to_owned())),
                (
                    "opencode".to_owned(),
                    Some("bizbrain/Qwen3.6-35B-A3B-thinking".to_owned())
                ),
            ],
            "primary then fallback, in order: {attempts:?}"
        );
    }

    /// CXA-F257 AC3: an attempt that errored before producing an outcome
    /// still names its engine, with the explicit unknown model — never a
    /// blank field downstream.
    #[tokio::test]
    async fn an_errored_attempt_names_its_engine_with_an_unknown_model() {
        let dies = Fixed {
            id: "dies",
            engine: "",
            model: "",
            stderr: "",
            code: 1,
            fail: true,
        };
        let fallback = Fixed {
            id: "claude",
            engine: "claude",
            model: "opus",
            stderr: "",
            code: 0,
            fail: false,
        };
        let chain = super::FailoverEngine::new(vec![dies, fallback]);
        let o = chain.run(request()).await.expect("fallback succeeds");
        assert_eq!(o.attempts.len(), 2, "{:?}", o.attempts);
        assert_eq!(o.attempts[0].engine, "dies");
        assert_eq!(o.attempts[0].model, None, "no outcome, no model id");
        assert_eq!(o.attempts[1].engine, "claude");
    }

    /// The metering decorator on TOP of the failover chain folds the whole
    /// trail — this is the exact production stack shape
    /// (`MeteringEngine<TranscriptEngine<RoutingEngine<FailoverEngine<_>>>>`),
    /// so provenance capture sees every attempt, not just the last engine.
    #[tokio::test]
    async fn metering_over_failover_captures_the_whole_trail_per_step() {
        use coxagent_application::state::Spend;
        let meter: crate::engine::metering::Meter =
            std::sync::Arc::new(std::sync::Mutex::new(Spend::default()));
        let chain = super::FailoverEngine::new(vec![
            Fixed {
                id: "claude",
                engine: "claude",
                model: "opus",
                stderr: "quota exhausted",
                code: 1,
                fail: false,
            },
            Fixed {
                id: "opencode",
                engine: "opencode",
                model: "bizbrain/Qwen3.6-35B-A3B-thinking",
                stderr: "",
                code: 0,
                fail: false,
            },
        ]);
        let eng = MeteringEngine::new(chain, std::sync::Arc::clone(&meter));
        let mut req = request();
        req.label = Some("CXA-F257".to_owned());
        eng.run(req).await.expect("runs");
        let m = meter.lock().expect("lock");
        assert_eq!(m.step_provenance.len(), 1, "one run, one step record");
        let step = &m.step_provenance[0];
        assert_eq!(step.ticket.as_deref(), Some("CXA-F257"));
        let engines: Vec<&str> = step.step.attempts.iter().map(|a| a.engine.as_str()).collect();
        assert_eq!(engines, vec!["claude", "opencode"], "the whole trail rides");
    }

    #[test]
    fn stamp_trail_replaces_nothing_but_attempts() {
        let o = AgentOutcome {
            engine: "opencode".to_owned(),
            model: "m".to_owned(),
            ..AgentOutcome::default()
        };
        let stamped = stamp_trail(
            o,
            vec![coxagent_application::state::EngineAttempt {
                engine: "claude".to_owned(),
                model: None,
            }],
        );
        assert_eq!(stamped.attempts.len(), 1);
        assert_eq!(stamped.engine, "opencode");
        assert_eq!(stamped.model, "m");
    }
}
