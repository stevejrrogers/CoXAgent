//! Project-delete tombstone (CXA-C023).
//!
//! `SqlStateStore::delete` previously guarded late writes with an in-process
//! [`std::sync::atomic::AtomicBool`] only, so a phase-end save from another
//! process — or from a process that never saw the delete — could re-INSERT the
//! purged row and resurrect the deleted project under a recreated id. The
//! tombstone closes that hole at the database: arming it commits in the SAME
//! transaction as the row purge, and every write path filters on its absence,
//! so any store instance anywhere is refused the moment the delete commits.
//!
//! The tombstone is cleared ONLY in `SqlStateStore::connect`: connecting is the
//! one moment an id legitimately comes back to life (hub boot of a registered
//! project, or re-registration after delete). Zombies never reconnect, so the
//! per-write guard keeps catching them; clearing anywhere else would either
//! resurrect deleted projects or brick one whose delete crashed between the
//! purge and the registry update.

use coxagent_application::PortError;
use tokio_postgres::Client;

/// Schema for the tombstone table. Idempotent; run on connect by the SQL
/// adapter's `migrate()` (its own batch beside `INIT_SQL`).
pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS project_tombstone (
    project_id  TEXT PRIMARY KEY,
    deleted_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    reason      TEXT NOT NULL DEFAULT ''
);";

/// The single guarded upsert behind every aggregate write. Both the INSERT
/// branch and the ON CONFLICT branch filter on the tombstone's absence, so the
/// "was this project deleted?" check and the revision check are decided
/// atomically inside ONE statement — no lock regime needed for READ COMMITTED.
/// Zero rows affected means "refused or stale"; the caller disambiguates with
/// a fresh tombstone re-check.
pub(crate) const GUARDED_SAVE_SQL: &str = "
INSERT INTO project_state (project_id, schema_version, revision, data)
SELECT $1, $2, 1, $3
 WHERE NOT EXISTS (SELECT 1 FROM project_tombstone WHERE project_id = $1)
ON CONFLICT (project_id) DO UPDATE
    SET data = EXCLUDED.data,
        schema_version = EXCLUDED.schema_version,
        revision = project_state.revision + 1,
        updated_at = now()
    WHERE project_state.revision = $4
      AND NOT EXISTS (SELECT 1 FROM project_tombstone WHERE project_id = $1)";

/// Arm the tombstone inside the delete transaction (same commit as the row
/// purge, so the two are inseparable). Re-arming an already-tombstoned id —
/// the idempotent second delete the contract suite exercises — refreshes the
/// instant and reason instead of failing the unique key.
///
/// # Errors
/// [`PortError::Backend`] when the statement fails; the whole delete
/// transaction must abort rather than purge without the tombstone armed.
pub(crate) async fn arm(
    tx: &tokio_postgres::Transaction<'_>,
    project_id: &str,
    reason: &str,
) -> Result<(), PortError> {
    tx.execute(
        "INSERT INTO project_tombstone (project_id, reason) VALUES ($1, $2)
         ON CONFLICT (project_id) DO UPDATE
            SET deleted_at = now(), reason = EXCLUDED.reason",
        &[&project_id, &reason],
    )
    .await
    .map_err(|e| PortError::Backend(format!("arm tombstone: {e}")))?;
    Ok(())
}

/// Clear the tombstone for a project that is legitimately coming back: called
/// only from `SqlStateStore::connect` (see the module docs for why nowhere
/// else).
///
/// # Errors
/// [`PortError::Backend`] when the statement fails; connect must fail loudly
/// rather than silently keep a stale refusal armed.
pub(crate) async fn clear(client: &Client, project_id: &str) -> Result<(), PortError> {
    client
        .execute(
            "DELETE FROM project_tombstone WHERE project_id = $1",
            &[&project_id],
        )
        .await
        .map_err(|e| PortError::Backend(format!("clear tombstone: {e}")))?;
    Ok(())
}

/// Whether the delete tombstone for this project id is currently armed.
///
/// # Errors
/// [`PortError::Backend`] when the query fails.
pub(crate) async fn exists(client: &Client, project_id: &str) -> Result<bool, PortError> {
    let row = client
        .query_opt(
            "SELECT 1 FROM project_tombstone WHERE project_id = $1",
            &[&project_id],
        )
        .await
        .map_err(|e| PortError::Backend(format!("tombstone check: {e}")))?;
    Ok(row.is_some())
}

/// The refusal error for a write that hit an armed tombstone — the same
/// envelope the in-process `deleted` flag has always returned, so callers
/// cannot tell a local refusal from a cross-process one (and need not).
pub(crate) fn refusal(project_id: &str) -> PortError {
    PortError::Backend(format!("[{project_id}] write refused: project was deleted"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_guarded_save_filters_the_tombstone_in_both_upsert_branches() {
        // The insert branch must not fire over a tombstone, and the
        // conflict-update branch must re-check it too — otherwise a writer
        // racing a delete could still bump the purged row back into existence
        // through the ON CONFLICT path.
        let occurrences = GUARDED_SAVE_SQL.matches("NOT EXISTS").count();
        assert_eq!(
            occurrences, 2,
            "the tombstone guard must gate both the INSERT and the ON CONFLICT branch: {GUARDED_SAVE_SQL}"
        );
        assert!(
            GUARDED_SAVE_SQL.contains("WHERE project_state.revision = $4"),
            "the revision predicate is unchanged: {GUARDED_SAVE_SQL}"
        );
    }

    #[test]
    fn the_refusal_reuses_the_write_refused_envelope() {
        let err = refusal("p1");
        assert!(
            err.to_string()
                .contains("write refused: project was deleted"),
            "same envelope as the in-process deleted flag: {err}"
        );
    }
}
