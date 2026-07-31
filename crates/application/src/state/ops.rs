// Part of the `state` module split by bounded context — see state/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Deploy and outage records: what shipped, what is healthy, what broke.

use serde::{Deserialize, Serialize};

use super::*;

/// One deployment: a version and the ticket that produced it. The changelog is
/// rendered from these — deterministic, zero-token, never out of sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRecord {
    pub version: SemVer,
    pub ticket: TicketId,
    pub title: String,
    /// RFC3339 timestamp.
    pub at: String,
}

/// A live engine-infrastructure problem: expired auth, a model the provider
/// rejected, a quota wall. These are not ticket failures and not the team's
/// fault, but they stop everything — so they are surfaced as an open incident
/// with the reason, the role that hit it, and when, and cleared the moment a
/// run of that engine succeeds again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineIncident {
    /// Engine id (`claude`, `opencode`).
    pub engine: String,
    /// The decisive line from the failure, already trimmed.
    pub reason: String,
    /// Role label that hit it first (`DEV-BUG`).
    pub role: String,
    pub since: String,
    /// How many runs have failed this way since `since`.
    pub hits: u32,
}

/// The last deployment outcome, surfaced on the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployStatus {
    pub at: String,
    pub ok: bool,
    pub summary: String,
    /// The commit sha this deploy attempt built/ran (absent when git isn't
    /// wired up), so "what's currently live" is provable rather than assumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    /// Result of the post-deploy health-endpoint probe for this attempt
    /// (COX-F005). `None` until the health check runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheckResult>,
}

/// Outcome of a single health-endpoint probe (COX-F005): the app's health
/// endpoint on the deployed port, checked with a bounded timeout before a
/// deploy can be marked successful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheckResult {
    /// Whether the endpoint answered healthy within the bound.
    pub passed: bool,
    /// HTTP status code returned, if the endpoint was reachable at all.
    pub http_status: Option<u16>,
    /// Round-trip time of the probe, in milliseconds.
    pub response_time_ms: Option<u64>,
}

/// The most recent deploy that passed both `deploy()` and `run_tests()` —
/// auto-rollback's target. Backed by the durable `refs/coxagent/last-good`
/// git ref, which survives ticket-branch deletion after a squash-merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownGoodDeploy {
    pub sha: String,
    /// RFC3339 timestamp.
    pub at: String,
    /// Ordinal of this success — the Nth deploy to pass both gates.
    pub deploy_index: u64,
    pub summary: String,
}

/// Outcome of the most recent auto-rollback attempt, surfaced on the
/// dashboard distinctly from a normal deploy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackStatus {
    /// RFC3339 timestamp.
    pub at: String,
    /// What triggered it, e.g. `"deploy failed"` / `"tests failed"`.
    pub reason: String,
    /// The commit sha rolled back to.
    pub to_sha: String,
    pub ok: bool,
    pub summary: String,
    /// The known-good deploy was older than `max_rollback_age_secs` — rollback
    /// was skipped (not attempted), not just unlucky.
    #[serde(default)]
    pub stale: bool,
    /// Code touching `migration_detection_paths` shipped since the known-good
    /// deploy — rolling the app back without the DB schema could be unsafe,
    /// so rollback was skipped (not attempted).
    #[serde(default)]
    pub migration_blocked: bool,
}

/// One queued execution job (control plane → runner). The hub NEVER executes
/// these itself when a live runner exists — execution stays on the execution
/// plane. Claim-and-remove is atomic via `mutate_state`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingJob {
    pub id: String,
    /// "force_merge" today; the kind namespace is owned by `contracts::JobSpec`.
    pub kind: String,
    #[serde(default)]
    pub args: serde_json::Value,
    pub queued_at: String,
    #[serde(default)]
    pub queued_by: String,
}

#[cfg(test)]
mod engine_incident_tests {
    use super::ProjectState;

    #[test]
    fn an_outage_is_raised_once_counted_and_closes_with_its_history() {
        let mut s = ProjectState::default();
        s.open_engine_incident("claude", "DevBug", "Failed to authenticate: OAuth expired");
        s.open_engine_incident("claude", "Test", "Failed to authenticate: OAuth expired");
        assert_eq!(s.engine_incidents.len(), 1, "one outage, not one per run");
        assert_eq!(s.engine_incidents[0].hits, 2);
        assert_eq!(s.engine_incidents[0].role, "DevBug", "who hit it first");

        // A second engine is its own incident.
        s.open_engine_incident("opencode", "DevFeature", "model not found");
        assert_eq!(s.engine_incidents.len(), 2);

        let closed = s.close_engine_incident("claude").expect("was open");
        assert_eq!(closed.hits, 2, "the caller can say how long it lasted");
        assert!(s.engine_incidents.iter().all(|i| i.engine != "claude"));
        assert!(
            s.close_engine_incident("claude").is_none(),
            "closing twice is not an event"
        );
    }
}
