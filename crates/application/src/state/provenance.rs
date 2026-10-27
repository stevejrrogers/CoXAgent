// Part of the `state` module split by bounded context — see state/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Per-ticket engine & model provenance (CXA-F257): which engine and model
//! ACTUALLY executed each agent step on a ticket — post-failover, post-
//! escalation — so the human verify gate approves evidence of the work, not
//! an assumption about what produced it.

use serde::{Deserialize, Serialize};

use super::*;

/// One engine/model try within a step. `engine` is the CLI that executed
/// (`AgentOutcome.engine`'s own contract); `model` is the id the adapter
/// stamped, or `None` when the engine cannot report one — `None` is the
/// explicit unknown, an empty string never is (see
/// [`crate::engine_provenance::attempt_label`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineAttempt {
    pub engine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One agent step's provenance, in the shape of the activity entry it
/// belongs to (`at`/`role`/`action`) plus the engine/model attempts that
/// executed it, in run order — primary first, then any failover (CXA-F257
/// AC2). Chronological, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepProvenance {
    pub at: String,
    pub role: String,
    pub action: String,
    pub attempts: Vec<EngineAttempt>,
}

/// A run captured by the `MeteringEngine` decorator, waiting in the spend
/// meter for the cycle's `drain_meter` fold. `ticket` is the run's label (a
/// TicketId) or `None` for runs that are not per-ticket work (ceremonies,
/// session resumes) — the fold drops those: provenance is per-ticket work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeteredStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    pub step: StepProvenance,
}

/// One metered run's DOLLAR charge, carried beside [`MeteredStep`] so
/// `drain_meter` can attribute actual cost to the ticket whose label the
/// request bore (CXA-F377). A `$0` charge (engine reported no usage) still
/// counts its run — a zero-usage run is visible, not invisible. A run that
/// died on an engine/infra fault (see `faults::is_infra_fault`) carries
/// `infra_fault = true`: its (partial, pre-crash) dollars are charged to
/// infra — excluded from the ticket's work figure at read time, never
/// silently dropped. Labeled `#[serde(default)]` end-to-end like every other
/// additive money field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeteredCharge {
    /// Request label — the ticket id (`None` = maintenance/unlabeled run ⇒
    /// unattributed bucket, flagged, never dropped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    /// Dashboard role label of the run.
    pub role: String,
    /// Engine that executed (empty ⇒ "unknown" at fold time).
    #[serde(default)]
    pub engine: String,
    /// Dollars this run consumed (0.0 when the engine reports nothing).
    #[serde(default)]
    pub cost_usd: f64,
    /// Engine/infra-side death — charge excluded from the work figure.
    #[serde(default)]
    pub infra_fault: bool,
}

/// Bounded per ticket (the `add_evidence` pattern): the newest steps stay,
/// the oldest evict, so a long-lived ticket cannot grow state without end.
pub const MAX_STEP_PROVENANCE: usize = 12;

impl ProjectState {
    /// Append one step's provenance to its ticket's bounded log — oldest
    /// first, newest kept. Called by the cycle's meter fold (`drain_meter`),
    /// the single writer of this record.
    pub fn record_step_provenance(&mut self, ticket: &str, step: StepProvenance) {
        let log = self
            .ticket_step_provenance
            .entry(ticket.to_owned())
            .or_default();
        log.push(step);
        let overflow = log.len().saturating_sub(MAX_STEP_PROVENANCE);
        if overflow > 0 {
            log.drain(0..overflow);
        }
    }

    /// The ticket's provenance log, chronological (oldest first). Empty —
    /// never a guess — for tickets with no captured runs.
    #[must_use]
    pub fn step_provenance(&self, ticket: &str) -> &[StepProvenance] {
        match self.ticket_step_provenance.get(ticket) {
            Some(log) => log,
            None => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(at: &str) -> StepProvenance {
        StepProvenance {
            at: at.to_owned(),
            role: "DEV-BUG".to_owned(),
            action: "agent run".to_owned(),
            attempts: vec![EngineAttempt {
                engine: "claude".to_owned(),
                model: Some("opus".to_owned()),
            }],
        }
    }

    /// The per-ticket bound mirrors `add_evidence`: at [`MAX_STEP_PROVENANCE`]
    /// entries the OLDEST evict, so a hot ticket's log stays bounded and the
    /// most recent attempts — the ones a verifier compares — survive.
    #[test]
    fn provenance_is_bounded_per_ticket_evicting_the_oldest() {
        let mut s = ProjectState::default();
        for i in 0..(MAX_STEP_PROVENANCE + 3) {
            s.record_step_provenance("CXA-B1", step(&format!("2026-08-30T10:{i:02}:00Z")));
        }
        let log = s.step_provenance("CXA-B1");
        assert_eq!(log.len(), MAX_STEP_PROVENANCE);
        assert_eq!(
            log[0].at, "2026-08-30T10:03:00Z",
            "the three oldest evicted, the newest kept"
        );
        assert_eq!(log.last().expect("non-empty").at, "2026-08-30T10:14:00Z");
        assert!(
            s.step_provenance("CXA-OTHER").is_empty(),
            "an untouched ticket reads as empty, never a guess"
        );
    }
}
