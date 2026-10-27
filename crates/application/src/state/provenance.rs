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

/// Bounded per ticket (the `add_evidence` pattern): the newest steps stay,
/// the oldest evict, so a long-lived ticket cannot grow state without end.
pub const MAX_STEP_PROVENANCE: usize = 12;

/// One run's DOLLAR delta captured by the `MeteringEngine`, waiting in the
/// spend meter for the cycle's `drain_meter` fold into per-ticket cost
/// attribution (CXA-B328). Parallel to [`MeteredStep`] — same
/// since-last-drain lifecycle, dropped identically when unlabeled — but it
/// names money instead of engines. Zero-usage runs still record an entry — a
/// zero-cost run is visible, not invisible. A run that died on an engine/infra
/// fault (`crate::faults::is_infra_fault`) carries `infra_fault = true`: its
/// (partial, pre-crash) dollars belong to infra, excluded from the ticket's
/// work figure at read time — never silently dropped as "not the ticket's
/// fault" (the house rule).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MeteredCharge {
    /// Run label — the ticket id, or `None` for non-ticket runs (ceremonies,
    /// session resumes): those are dropped at the fold like their provenance
    /// peers.
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
    /// The run died on an engine/infra fault — charged to infra, excluded
    /// from the ticket's work figure.
    #[serde(default)]
    pub infra_fault: bool,
}

/// Bounded per ticket (same rationale as [`MAX_STEP_PROVENANCE`]): the most
/// recent charges survive; a hot ticket cannot grow state without end.
pub const MAX_TICKET_CHARGES: usize = 12;

impl ProjectState {
    /// Charge one run's dollar delta to its ticket's bounded ledger — called
    /// ONLY by the cycle's `drain_meter` fold, the single writer.
    pub fn record_ticket_charge(&mut self, ticket: &str, charge: MeteredCharge) {
        let log = self.ticket_charges.entry(ticket.to_owned()).or_default();
        log.push(charge);
        let overflow = log.len().saturating_sub(MAX_TICKET_CHARGES);
        if overflow > 0 {
            log.drain(0..overflow);
        }
    }

    /// The ticket's charge ledger, chronological (oldest first), EXCLUDING
    /// infra-fault charges — they belong to infra because an engine/infra
    /// fault is not the ticket's fault (house rule). Empty — never a guess —
    /// for tickets with no completed runs.
    #[must_use]
    pub fn ticket_charges_excluding_infra(&self, ticket: &str) -> Vec<&MeteredCharge> {
        self.ticket_charges
            .get(ticket)
            .map(|log| log.iter().filter(|c| !c.infra_fault).collect())
            .unwrap_or_default()
    }

    /// Total dollars charged to the ticket across completed (non-infra-fault)
    /// runs.
    #[must_use]
    pub fn ticket_cost_usd(&self, ticket: &str) -> f64 {
        self.ticket_charges_excluding_infra(ticket)
            .iter()
            .map(|c| c.cost_usd)
            .sum()
    }

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

    fn charge(usd: f64) -> MeteredCharge {
        MeteredCharge {
            ticket: Some("CXA-B1".to_owned()),
            role: "DEV-FEATURE".to_owned(),
            engine: "claude".to_owned(),
            cost_usd: usd,
            infra_fault: false,
        }
    }

    /// CXA-B328 regression: drain_meter folds every run's dollar delta into
    /// the per-ticket ledger — zero-cost runs included (a zero-usage run is
    /// visible, not invisible), and an engine/infra-fault run still leaves its
    /// entry so nothing vanishes.
    #[test]
    fn charges_are_recorded_per_ticket_including_zero_cost_runs() {
        let mut s = ProjectState::default();
        s.record_ticket_charge("CXA-B1", charge(0.20));
        s.record_ticket_charge("CXA-B1", charge(0.0));
        assert_eq!(s.ticket_charges["CXA-B1"].len(), 2);
        assert_eq!(s.step_provenance("CXA-B1").len(), 0, "independent ledger");
    }

    /// The work figure excludes infra-fault dollars without dropping them:
    /// they stay in the raw ledger (`ticket_charges`), only the read view
    /// filters — money that exists is never silently discarded.
    #[test]
    fn infra_fault_dollars_stay_in_the_ledger_but_leave_the_work_figure() {
        let mut s = ProjectState::default();
        s.record_ticket_charge("CXA-B1", charge(0.30));
        s.record_ticket_charge(
            "CXA-B1",
            MeteredCharge {
                cost_usd: 0.70,
                infra_fault: true,
                ..charge(0.0)
            },
        );
        assert!((s.ticket_cost_usd("CXA-B1") - 0.30).abs() < 1e-9);
        assert_eq!(s.ticket_charges["CXA-B1"].len(), 2);
    }

    /// Bounded like provenance: at [`MAX_TICKET_CHARGES`] entries the oldest
    /// evict; a hot ticket cannot grow state without end.
    #[test]
    fn charges_are_bounded_per_ticket_evicting_the_oldest() {
        let mut s = ProjectState::default();
        for i in 0..(MAX_TICKET_CHARGES + 3) {
            #[allow(clippy::cast_precision_loss)]
            let usd = i as f64;
            s.record_ticket_charge("CXA-B1", charge(usd));
        }
        let log = &s.ticket_charges["CXA-B1"];
        assert_eq!(log.len(), MAX_TICKET_CHARGES);
        assert!(
            (log[0].cost_usd - 3.0).abs() < 1e-9,
            "the three oldest evicted"
        );
    }
}
