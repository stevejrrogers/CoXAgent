//! Quarantine ledger and write-back gate (CXA-F229).
//!
//! When the structural-integrity audit refuses a mutation at the write
//! boundary, the attempted payload is recorded here instead of being silently
//! persisted: the corruption that would have poisoned goal-line attribution
//! stays inspectable, and the hub surfaces the ledger on the overview. The
//! ledger is diagnostic, never authoritative — recording a quarantine entry
//! must not mask the refusal error that caused it. [`gate_save`] is the
//! shared write-boundary gate both store adapters call before persisting.

use coxagent_application::ports::outbound::QuarantineEntry;
use coxagent_application::state::{ProjectState, StateIntegrityAuditor};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Keep the ledger bounded: the newest 50 refusals.
pub(crate) const MAX_QUARANTINE: usize = 50;
/// Cap a quarantined payload — enough to diagnose a corrupted document
/// without turning the ledger into a second state store.
const MAX_PAYLOAD_CHARS: usize = 20_000;
/// Ledger file kept beside `state.json` (hidden like `.state.lock`).
const QUARANTINE_FILE: &str = ".state.quarantine.json";

/// Bounded, newest-last ledger of refused write-backs. File-backed when the
/// adapter owns a directory (JSON store), memory-only otherwise (SQL store —
/// Postgres keeps no extra table; refusals there are also logged).
pub(crate) struct QuarantineLedger {
    path: Option<PathBuf>,
    entries: Mutex<Vec<QuarantineEntry>>,
}

impl QuarantineLedger {
    /// File-backed ledger in the state directory, seeded from any previous
    /// ledger file (a corrupt file yields an empty ledger — this is a
    /// diagnostic trail, not state).
    pub(crate) fn in_dir(dir: &Path) -> Self {
        let path = dir.join(QUARANTINE_FILE);
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<QuarantineEntry>>(&bytes).ok())
            .unwrap_or_default();
        Self {
            path: Some(path),
            entries: Mutex::new(entries),
        }
    }

    /// Memory-only ledger for adapters without a directory of their own.
    pub(crate) fn memory_only() -> Self {
        Self {
            path: None,
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Record one refused write: audit the payload's first findings into the
    /// entry, cap the payload, keep the ledger bounded, and persist the file
    /// when there is one — best-effort, never failing the caller.
    pub(crate) fn record_refusal(
        &self,
        violation: &coxagent_application::state::IntegrityViolation,
        payload: &ProjectState,
    ) {
        let rule_id = violation
            .findings
            .first()
            .map(|f| f.rule_id.clone())
            .unwrap_or_default();
        let detail: String = violation.to_string().chars().take(2_000).collect();
        let payload: String = serde_json::to_string(payload)
            .unwrap_or_default()
            .chars()
            .take(MAX_PAYLOAD_CHARS)
            .collect();
        let entry = QuarantineEntry {
            at: coxagent_application::state::now_rfc3339(),
            rule_id,
            detail,
            payload,
        };
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.push(entry);
        let overflow = entries.len().saturating_sub(MAX_QUARANTINE);
        if overflow > 0 {
            entries.drain(0..overflow);
        }
        if let Some(path) = &self.path {
            if let Ok(json) = serde_json::to_vec_pretty(&*entries) {
                if let Err(e) = std::fs::write(path, json) {
                    tracing::warn!("quarantine ledger write failed: {e}");
                }
            }
        }
    }

    /// The newest entries, oldest first (bounded by [`MAX_QUARANTINE`]).
    pub(crate) fn recent(&self) -> Vec<QuarantineEntry> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// The write-boundary gate shared by the store adapters: validate (existing
/// semantics, untouched), then audit structural integrity. A refused payload
/// is recorded in `ledger` before the error returns, so the corruption that
/// would have been persisted is inspectable instead of silent.
///
/// # Errors
/// [`PortError::Corrupt`] naming the failed invariants — the same error
/// envelope the pre-existing validation uses.
pub(crate) fn gate_save(
    state: &ProjectState,
    ledger: &QuarantineLedger,
) -> Result<(), coxagent_application::PortError> {
    state.validate().map_err(|e| {
        coxagent_application::PortError::Corrupt(format!("refusing to save invalid state: {e}"))
    })?;
    if let Err(violation) = StateIntegrityAuditor::check(state) {
        ledger.record_refusal(&violation, state);
        return Err(coxagent_application::PortError::Corrupt(format!(
            "refusing to save state failing structural integrity audit: {violation}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use coxagent_application::state::Evidence;

    fn state_with_dangling_evidence() -> ProjectState {
        let mut state = ProjectState::default();
        state.ticket_evidence.insert(
            "CXA-F999".to_owned(),
            vec![Evidence {
                kind: "test".to_owned(),
                label: "orphan".to_owned(),
                detail: "d".to_owned(),
                at: String::new(),
            }],
        );
        state
    }

    #[test]
    fn a_refused_payload_is_recorded_and_bounded() {
        let ledger = QuarantineLedger::memory_only();
        let bad = state_with_dangling_evidence();
        for _ in 0..(MAX_QUARANTINE + 5) {
            let violation = StateIntegrityAuditor::check(&bad).unwrap_err();
            ledger.record_refusal(&violation, &bad);
        }
        let recent = ledger.recent();
        assert_eq!(recent.len(), MAX_QUARANTINE, "ledger stays bounded");
        assert!(recent[0].detail.contains("dangling_ticket_reference"));
        assert!(recent[0].payload.contains("ticket_evidence"));
    }

    #[test]
    fn the_file_ledger_round_trips_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = QuarantineLedger::in_dir(dir.path());
        let violation = StateIntegrityAuditor::check(&state_with_dangling_evidence()).unwrap_err();
        ledger.record_refusal(&violation, &state_with_dangling_evidence());
        assert_eq!(ledger.recent().len(), 1);

        // A fresh instance over the same directory reads the trail back.
        let reopened = QuarantineLedger::in_dir(dir.path());
        assert_eq!(reopened.recent().len(), 1);
        assert_eq!(
            reopened.recent()[0].rule_id,
            coxagent_application::state::DANGLING_TICKET_REFERENCE
        );
    }

    #[test]
    fn gate_save_refuses_dangling_state_and_quarantines_it() {
        let ledger = QuarantineLedger::memory_only();
        let bad = state_with_dangling_evidence();
        let err = gate_save(&bad, &ledger).expect_err("refused");
        assert!(err.to_string().contains("structural integrity audit"));
        assert_eq!(ledger.recent().len(), 1);
    }

    #[test]
    fn gate_save_still_enforces_the_pre_existing_validation() {
        // A duplicate ticket id fails the older schema-level validation
        // BEFORE the integrity audit is consulted — semantics unchanged.
        use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};
        let mut state = ProjectState::default();
        for _ in 0..2 {
            state.tickets.push(
                Ticket::new(
                    TicketId::new("CXA-F001").unwrap(),
                    TicketType::Feature,
                    "t",
                    "",
                    Priority::Medium,
                    Complexity::Small,
                    false,
                )
                .unwrap(),
            );
        }
        let ledger = QuarantineLedger::memory_only();
        let err = gate_save(&state, &ledger).expect_err("refused");
        assert!(err.to_string().contains("refusing to save invalid state"));
        assert!(
            ledger.recent().is_empty(),
            "pre-integrity refusal is not quarantined"
        );
    }
}
