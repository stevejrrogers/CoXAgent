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

/// A human's verdict on one detected revert (CXA-F047). Detection is a
/// heuristic over commit subjects, so a `Revert` of a docs bump or a CI
/// change would look identical to reverted shipped work — the decision is
/// what turns a suspicion into a fact the loop is allowed to learn from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevertDecision {
    #[default]
    Pending,
    Approved,
    Dismissed,
}

/// One merged-then-reverted work event (CXA-F047): git history shows a commit
/// undoing work this team shipped, attributed to the ticket and the agent role
/// that produced it. First-class state (alongside `history`) so approval
/// survives restarts and re-scans never re-flag what a human already decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertEvent {
    /// The revert commit's sha — the identity a re-scan dedupes against.
    pub sha: String,
    /// The revert commit's subject, verbatim.
    pub subject: String,
    /// The shipping ticket id (resolved from deploy history).
    pub ticket: String,
    /// Role label that shipped the ticket (`DEV-FEATURE`).
    pub role: String,
    /// RFC3339 commit date of the revert.
    pub reverted_at: String,
    /// RFC3339 when the scan detected it.
    pub detected_at: String,
    pub decision: RevertDecision,
    /// RFC3339 when the human decided (absent while pending).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    /// Who decided (username, or the runner for auto-dismissals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
}

/// Keep the revert ledger bounded.
pub const MAX_REVERT_EVENTS: usize = 100;

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
    /// CXA-F289: when this attempt FAILED, the size-capped forensics bundle
    /// the deploy adapter captured at the failure site (compose stderr tail +
    /// recent per-container logs), so the record carries its own evidence
    /// instead of a one-line summary. Absent for successful/skipped attempts
    /// and for records written before this existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_bundle: Option<DeployFailureBundle>,
}

/// One container's recent log tail, attributed to its compose service, inside
/// a [`DeployFailureBundle`] (CXA-F289).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerLogTail {
    /// The compose service the log lines came from.
    pub service: String,
    /// The recent log tail (bounded by [`DeployFailureBundle::new`]).
    pub tail: String,
}

/// The size-capped forensics bundle a FAILED deploy attempt attaches to its
/// record (CXA-F289): the compose stderr tail plus the recent logs of every
/// container the compose project managed, so a deploy failure is diagnosed
/// from the record instead of host archaeology. Secrets are masked and the
/// whole bundle bounded by [`DeployFailureBundle::new`] — the persisted state
/// is broadcast to every dashboard once a second, so a chatty build must not
/// be able to bloat it (bounded-state house rule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployFailureBundle {
    /// Tail of the failed `docker compose` invocation's own output.
    pub stderr_tail: String,
    /// Recent per-container logs, when any container existed.
    #[serde(default)]
    pub container_logs: Vec<ContainerLogTail>,
    /// AC3: set by the adapter when compose failed BEFORE any container
    /// existed — the record explicitly states there are no container logs
    /// rather than rendering an empty log section.
    #[serde(default)]
    pub no_container_logs: bool,
}

/// Hard cap on one persisted [`DeployFailureBundle`], serialized.
pub const MAX_FAILURE_BUNDLE_BYTES: usize = 131_072;

/// Compose stderr tail: keep at most this many lines (CXA-F289 design).
const BUNDLE_STDERR_MAX_LINES: usize = 400;

/// Per-container log tail: keep at most this many lines (`--tail 200`).
const BUNDLE_CONTAINER_MAX_LINES: usize = 200;

/// Whether an env-assignment key names a credential. Shared rule for the
/// secret-shaped masking (AC4) — mirrored by the dashboard's renderer so a
/// bundle is masked again before it ever reaches a browser.
fn is_secret_key(key: &str) -> bool {
    let key = key.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    [
        "password",
        "passwd",
        "pwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "credential",
        "private_key",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

/// CXA-F289 AC4: mask the VALUES of secret-shaped `KEY=value` assignments so
/// a credential never persists or renders inside a forensics bundle. Pure —
/// the adapter, the persist path and the dashboard renderer share this one
/// rule instead of each re-deriving it.
#[must_use]
pub fn redact_secret_shaped(text: &str) -> String {
    text.lines()
        .map(|line| match line.find('=') {
            Some(eq) if is_secret_key(&line[..eq]) => format!("{}=***", &line[..eq]),
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Keep only the last `max` LINES of `s` — the newest output is what
/// diagnoses a failure.
fn tail_lines(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(max);
    lines[start..].join("\n")
}

/// Keep only the last `max` BYTES of `s`, cut on a char boundary.
fn tail_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut at = s.len() - max;
    while !s.is_char_boundary(at) {
        at += 1;
    }
    s[at..].to_owned()
}

impl DeployFailureBundle {
    /// Build a bundle from the adapter's raw capture material: mask
    /// secret-shaped values first (a cap that cuts text must never be able to
    /// cut a credential in half — masked output is safe to truncate), then
    /// bound the whole serialized bundle to [`MAX_FAILURE_BUNDLE_BYTES`].
    #[must_use]
    pub fn new(
        stderr_tail: &str,
        container_logs: Vec<ContainerLogTail>,
        no_container_logs: bool,
    ) -> Self {
        let mut bundle = Self {
            stderr_tail: redact_secret_shaped(&tail_lines(stderr_tail, BUNDLE_STDERR_MAX_LINES)),
            container_logs: container_logs
                .into_iter()
                .map(|c| ContainerLogTail {
                    service: c.service,
                    tail: redact_secret_shaped(&tail_lines(&c.tail, BUNDLE_CONTAINER_MAX_LINES)),
                })
                .collect(),
            no_container_logs,
        };
        bundle.bound();
        bundle
    }

    /// Enforce the cap: tails are cut first (stderr to half the budget, each
    /// container tail to an eighth), then — if a pile of container tails
    /// still overflows — the OLDEST container log is dropped until it fits.
    /// stderr alone can never exceed half the cap, so this converges.
    fn bound(&mut self) {
        self.stderr_tail = tail_bytes(&self.stderr_tail, MAX_FAILURE_BUNDLE_BYTES / 2);
        for log in &mut self.container_logs {
            log.tail = tail_bytes(&log.tail, MAX_FAILURE_BUNDLE_BYTES / 8);
        }
        while self.serialized_len() > MAX_FAILURE_BUNDLE_BYTES && !self.container_logs.is_empty() {
            self.container_logs.remove(0);
        }
    }

    fn serialized_len(&self) -> usize {
        serde_json::to_string(self).map_or(usize::MAX, |s| s.len())
    }
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
    /// CXA-F289: the deploy-failure forensics attached to THIS rollback
    /// record — the triggering attempt's bundle when the rollback succeeded
    /// (a successful rollback overwrites the deploy record), or the failed
    /// rollback redeploy's own bundle when the rollback itself failed, so a
    /// failed deploy and a failed rollback stay two separately inspectable
    /// attempt records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_bundle: Option<DeployFailureBundle>,
}

/// One durable incident record (CXA-F012): after every auto-rollback or
/// rollback-skip the loop writes a single inspection-grade entry linking the
/// failing commit, what it was rolled back to (or why it wasn't), and any
/// root-cause prevention ticket + team lesson — turning each outage into one
/// "why + fix-the-root" cycle instead of a revert-and-forget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentRecord {
    /// RFC3339 timestamp.
    pub at: String,
    /// What triggered it (`deploy failed` / `tests failed` / …).
    pub reason: String,
    /// The commit sha that shipped and broke the health probe / tests.
    pub failed_sha: String,
    /// The sha rolled back to; empty when rollback was skipped.
    #[serde(default)]
    pub to_sha: String,
    /// Whether the rollback itself succeeded (`false` when skipped/failed).
    pub ok: bool,
    /// Human-readable summary of what happened.
    pub summary: String,
    /// Id of the deduped root-cause prevention ticket filed for this incident.
    #[serde(default)]
    pub root_cause_ticket: Option<String>,
    /// A team lesson recorded from this incident (deduped against lessons).
    #[serde(default)]
    pub lesson: Option<String>,
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

/// One open loop-liveness stall episode (CXA-F259): the persisted record the
/// hub-side watchdog dedupes against, so the same stall alerts once — not
/// once per sweep — survives a hub restart, and self-clears when new activity
/// lands. Written only on alert open/escalate/clear (a handful of CAS-safe
/// writes per day, never per sweep). serde-defaulted so state persisted
/// before this existed loads untouched — the additive-field convention.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StallEpisode {
    /// RFC3339 when this episode's stall was first detected.
    pub since: String,
    /// The stale worker the alert named (`account@host`, or `"none"` when the
    /// registry had no live worker left — the process is gone).
    pub worker: String,
    /// RFC3339 of the newest activity-trail entry when the alert opened.
    pub last_activity_at: String,
    /// RFC3339 of the most recent alert (open or escalation) — the dedupe
    /// clock the escalation horizon measures from.
    pub last_alert_at: String,
    /// How many escalation re-alerts have fired for this episode.
    pub escalations: u32,
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

#[cfg(test)]
mod failure_bundle_tests {
    use super::{
        redact_secret_shaped, ContainerLogTail, DeployFailureBundle, MAX_FAILURE_BUNDLE_BYTES,
    };

    #[test]
    fn secret_shaped_assignments_are_masked_but_plain_lines_are_not() {
        let out = redact_secret_shaped(
            "PG_PASSWORD=hunter2\ndocker compose up failed: exit 1\nAPI_TOKEN=abc123\nNOTE=plain text",
        );
        assert_eq!(
            out,
            "PG_PASSWORD=***\ndocker compose up failed: exit 1\nAPI_TOKEN=***\nNOTE=plain text",
            "secret VALUES masked, keys and ordinary lines untouched"
        );
    }

    #[test]
    fn masking_runs_before_capping_so_a_cut_never_exposes_a_secret() {
        // A 10 MiB single secret line: the cap must cut the MASKED text, and
        // no slice of the raw value may survive anywhere in the bundle.
        let value = "z".repeat(10 * 1024 * 1024);
        let bundle =
            DeployFailureBundle::new(&format!("PG_PASSWORD={value}"), Vec::new(), true);
        assert!(
            !bundle.stderr_tail.contains('z'),
            "the raw secret value must not survive masking + capping"
        );
        assert!(bundle.stderr_tail.contains("PG_PASSWORD=***"));
    }

    #[test]
    fn the_whole_bundle_stays_within_the_persisted_cap() {
        let chatty = "build line with enough words to bulk up the log output\n".repeat(20_000);
        let logs: Vec<ContainerLogTail> = (0..40)
            .map(|i| ContainerLogTail {
                service: format!("svc{i}"),
                tail: chatty.clone(),
            })
            .collect();
        let bundle = DeployFailureBundle::new(&chatty, logs, false);
        let size = serde_json::to_string(&bundle).expect("serialize").len();
        assert!(
            size <= MAX_FAILURE_BUNDLE_BYTES,
            "a chatty build must not bloat the persisted state: {size} > {MAX_FAILURE_BUNDLE_BYTES}"
        );
        assert!(
            !bundle.no_container_logs,
            "the marker records the ADAPTER's observation, it is not derived from a trimmed list"
        );
    }

    #[test]
    fn tails_keep_the_newest_lines_and_bytes() {
        let long = (0..1000)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = super::tail_lines(&long, 2);
        assert_eq!(tail, "line998\nline999", "the tail is the NEWEST output");
        let cut = super::tail_bytes(&long, 10);
        assert!(long.ends_with(&cut), "the byte tail keeps the end");
        // A multi-byte char must never be cut in half: 3 bytes of 2-byte
        // chars yields ONE whole char (the cap is a maximum, not exact).
        let multibyte = "é".repeat(100);
        assert_eq!(super::tail_bytes(&multibyte, 3), "é");
    }
}
