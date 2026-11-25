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
use tokio_postgres::Client;

/// Schema for the durable quarantine ledger table (CXA-C023). Idempotent; run
/// on connect as part of the SQL adapter's migration. `seq` orders entries
/// within a project (the insert order); `at` is the server clock at insert.
pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS project_quarantine (
    project_id TEXT NOT NULL,
    seq        BIGSERIAL PRIMARY KEY,
    at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    rule_id    TEXT NOT NULL,
    detail     TEXT NOT NULL,
    payload    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS project_quarantine_recent
    ON project_quarantine (project_id, seq DESC);";

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
    /// when there is one — best-effort, never failing the caller. Returns the
    /// entry it recorded so a durable backing store (the SQL adapter's
    /// `project_quarantine` table) can persist the same entry.
    pub(crate) fn record_refusal(
        &self,
        violation: &coxagent_application::state::IntegrityViolation,
        payload: &ProjectState,
    ) -> QuarantineEntry {
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
        entries.push(entry.clone());
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
        entry
    }

    /// The newest entries, oldest first (bounded by [`MAX_QUARANTINE`]).
    pub(crate) fn recent(&self) -> Vec<QuarantineEntry> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// The outcome of a refused save: the error the caller must surface, plus the
/// ledger entry the refusal recorded. `quarantined` is `None` when the refusal
/// came from the pre-audit schema validation — that class is not quarantined
/// (only integrity-audit refusals are). The entry is boxed: it is the cold
/// path (refusals are exceptional) and keeps the error small on the hot one.
#[derive(Debug)]
pub(crate) struct RefusedSave {
    pub error: coxagent_application::PortError,
    pub quarantined: Option<Box<QuarantineEntry>>,
}

/// The write-boundary gate shared by the store adapters: validate (existing
/// semantics, untouched), then audit structural integrity. A refused payload
/// is recorded in `ledger` before the error returns, so the corruption that
/// would have been persisted is inspectable instead of silent.
///
/// # Errors
/// [`RefusedSave`] naming the failed invariants — the same error envelope the
/// pre-existing validation uses, plus the quarantined entry when one was
/// recorded.
pub(crate) fn gate_save(
    state: &mut ProjectState,
    ledger: &QuarantineLedger,
) -> Result<(), RefusedSave> {
    state.validate().map_err(|e| RefusedSave {
        error: coxagent_application::PortError::Corrupt(format!(
            "refusing to save invalid state: {e}"
        )),
        quarantined: None,
    })?;
    if let Err(violation) = StateIntegrityAuditor::check(state) {
        // Dangling ticket-keyed map entries have exactly one safe repair
        // (drop the entry), and the auditor ships a healer for them. Refusing
        // instead of healing bricked the whole hub on FIRST deploy: decades of
        // pre-auditor legacy keys ("2.26.2", renamed ticket ids) failed every
        // save, so nothing the agents did could persist. Heal that class in
        // place and save; anything else still refuses and quarantines.
        // Name WHAT dangled: the bare count recurred every ~20 minutes with
        // no way to trace which writer keeps minting orphaned keys.
        let dangling: Vec<String> = violation
            .findings
            .iter()
            .map(|f| {
                format!(
                    "{}: {}",
                    f.ticket_id.as_deref().unwrap_or("?"),
                    f.detail.chars().take(80).collect::<String>()
                )
            })
            .collect();
        match state.heal_dangling_references() {
            Ok(healed) if healed > 0 => {
                tracing::warn!(
                    "structural integrity audit: healed {healed} dangling ticket reference(s) at the write boundary: {}",
                    dangling.join(" | ")
                );
                return Ok(());
            }
            _ => {}
        }
        let quarantined = ledger.record_refusal(&violation, state);
        return Err(RefusedSave {
            error: coxagent_application::PortError::Corrupt(format!(
                "refusing to save state failing structural integrity audit: {violation}"
            )),
            quarantined: Some(Box::new(quarantined)),
        });
    }
    Ok(())
}

// --- Durable ledger backing (CXA-C023) --------------------------------------
//
// The SQL adapter persists every recorded refusal into `project_quarantine` so
// the trail survives a hub restart and is visible from every store instance —
// the file ledger does this for the JSON adapter, the table does it for the
// shared Postgres one. All three helpers are best-effort diagnostics: a
// failure is returned to the caller to log, never allowed to mask the refusal
// that produced the entry.

/// [`MAX_QUARANTINE`] as the Postgres BIGINT the LIMIT and prune parameter
/// expect — a checked conversion rather than a wrap-on-overflow cast.
fn max_quarantine_sql() -> i64 {
    i64::try_from(MAX_QUARANTINE).unwrap_or(i64::MAX)
}

/// Best-effort persist one recorded refusal and prune the project's ledger to
/// the newest [`MAX_QUARANTINE`] rows. `at` is left to the server clock
/// (DEFAULT now()), consistent with the `seq` ordering.
///
/// # Errors
/// [`PortError::Backend`] when the insert or prune fails — the caller logs it
/// and still returns the original refusal error.
pub(crate) async fn persist_entry(
    client: &Client,
    project_id: &str,
    entry: &QuarantineEntry,
) -> Result<(), coxagent_application::PortError> {
    client
        .execute(
            "INSERT INTO project_quarantine (project_id, rule_id, detail, payload)
             VALUES ($1, $2, $3, $4)",
            &[&project_id, &entry.rule_id, &entry.detail, &entry.payload],
        )
        .await
        .map_err(|e| coxagent_application::PortError::Backend(format!("quarantine insert: {e}")))?;
    client
        .execute(
            "DELETE FROM project_quarantine
              WHERE project_id = $1
                AND seq NOT IN (
                    SELECT seq FROM project_quarantine
                     WHERE project_id = $1
                     ORDER BY seq DESC
                     LIMIT $2
                )",
            &[&project_id, &max_quarantine_sql()],
        )
        .await
        .map_err(|e| coxagent_application::PortError::Backend(format!("quarantine prune: {e}")))?;
    Ok(())
}

/// Remove every quarantine row scoped to one project — called inside the
/// delete transaction so a purged project leaves no ledger rows behind.
///
/// # Errors
/// [`PortError::Backend`] when the delete fails; the whole delete transaction
/// must abort rather than half-purge.
pub(crate) async fn purge_project(
    tx: &tokio_postgres::Transaction<'_>,
    project_id: &str,
) -> Result<(), coxagent_application::PortError> {
    tx.execute(
        "DELETE FROM project_quarantine WHERE project_id = $1",
        &[&project_id],
    )
    .await
    .map_err(|e| coxagent_application::PortError::Backend(format!("quarantine purge: {e}")))?;
    Ok(())
}

/// The newest [`MAX_QUARANTINE`] durable entries for one project, oldest
/// first — the same ordering contract the in-memory ledger's
/// [`QuarantineLedger::recent`] keeps.
///
/// # Errors
/// [`PortError::Backend`] when the query fails; the caller falls back to its
/// in-memory buffer.
pub(crate) async fn load_recent(
    client: &Client,
    project_id: &str,
) -> Result<Vec<QuarantineEntry>, coxagent_application::PortError> {
    let rows = client
        .query(
            "SELECT to_char(at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), rule_id, detail, payload
               FROM project_quarantine
              WHERE project_id = $1
              ORDER BY seq DESC
              LIMIT $2",
            &[&project_id, &max_quarantine_sql()],
        )
        .await
        .map_err(|e| coxagent_application::PortError::Backend(format!("quarantine select: {e}")))?;
    let mut entries: Vec<QuarantineEntry> = rows
        .into_iter()
        .map(|r| QuarantineEntry {
            at: r.get(0),
            rule_id: r.get(1),
            detail: r.get(2),
            payload: r.get(3),
        })
        .collect();
    entries.reverse();
    Ok(entries)
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
                source_gates: Vec::new(),
                actor: String::new(),
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
        // Renamed: dangling references now HEAL at the boundary (the only
        // safe repair is dropping the entry) — the save succeeds and the
        // orphaned key is gone. Refusal is reserved for unhealable findings.
        let mut bad = state_with_dangling_evidence();
        let dir = tempfile::tempdir().expect("tmp");
        let ledger = QuarantineLedger::in_dir(dir.path());
        gate_save(&mut bad, &ledger).expect("healed and saved");
        assert!(bad.ticket_evidence.is_empty(), "dangling entry dropped");
        assert!(
            ledger.recent().is_empty(),
            "healed saves are not quarantined"
        );
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
        let refused = gate_save(&mut state, &ledger).expect_err("refused");
        assert!(refused
            .error
            .to_string()
            .contains("refusing to save invalid state"));
        assert!(
            refused.quarantined.is_none(),
            "pre-integrity refusal is not quarantined"
        );
        assert!(
            ledger.recent().is_empty(),
            "pre-integrity refusal is not quarantined"
        );
    }
}
