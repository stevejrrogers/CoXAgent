//! `SqlStateStore` — a Postgres-backed [`StateStorePort`] for multi-tenant
//! deployments. Since CXA-C019b the aggregate is stored as ONE row per
//! bounded-context shard (the C019a [`ShardKind`] vocabulary) plus a
//! head-revision row carrying the single optimistic `revision`, so a save
//! rewrites only the shards whose payload changed and `claim_ticket` locks
//! only the Tickets shard row — not the whole aggregate. Many projects share
//! one database while staying isolated by key.
//!
//! This is the port swap the architecture promised: use cases are unchanged;
//! only the adapter differs from [`super::JsonStateStore`]. Optimistic
//! concurrency (a monotonic `revision` on the head row) rejects lost updates
//! from two writers. A pre-C019b single-blob `project_state` row is migrated
//! to shards atomically (see [`Self::migrate_legacy_row_tx`]).

use async_trait::async_trait;
use coxagent_application::ports::outbound::{
    QuarantineEntry, StateStorePort, WorkerCaps, WorkerEntry,
};
use coxagent_application::state::{
    ProjectState, ShardData, ShardKind, ShardedState, SCHEMA_VERSION,
};
use coxagent_application::PortError;
use coxagent_domain::{Role, TicketId};
use deadpool_postgres::Pool;
use std::sync::Arc;

use super::quarantine::{gate_save, QuarantineLedger};
use super::tombstone;

/// Schema for the shared project + coordination tables. Idempotent; run on
/// connect. `project_coord` is the cross-machine coordination row set: one
/// `leader` per project and one lease per `(ticket, stage)`.
const INIT_SQL: &str = "
CREATE TABLE IF NOT EXISTS project_state (
    project_id     TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    revision       BIGINT  NOT NULL DEFAULT 0,
    data           JSONB   NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS project_coord (
    project_id TEXT NOT NULL,
    kind       TEXT NOT NULL,          -- 'leader' | 'stage' | 'worker'
    coord_key  TEXT NOT NULL,          -- '' leader, 'ticket|stage' lease, worker id
    worker     TEXT NOT NULL,
    at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    role       TEXT,                   -- worker registry: current role
    ticket     TEXT,                   -- worker registry: current ticket
    PRIMARY KEY (project_id, kind, coord_key)
);
ALTER TABLE project_coord ADD COLUMN IF NOT EXISTS role TEXT;
ALTER TABLE project_coord ADD COLUMN IF NOT EXISTS ticket TEXT;
-- Worker registry: the agent CLIs that runner found on its own PATH, comma
-- separated. The hub cannot detect these for a remote runner.
ALTER TABLE project_coord ADD COLUMN IF NOT EXISTS engines TEXT;
-- Worker registry: `provider/model` pairs that runner's opencode can reach,
-- newline separated. Custom providers exist only in the user's own CLI config.
ALTER TABLE project_coord ADD COLUMN IF NOT EXISTS models TEXT;
-- Worker registry: JSON result of probing git + forge access on that machine.
ALTER TABLE project_coord ADD COLUMN IF NOT EXISTS gitcheck TEXT;
-- Worker registry: that machine's OS + developer tooling, as JSON.
ALTER TABLE project_coord ADD COLUMN IF NOT EXISTS tooling TEXT;
-- Worker registry: the coxagent build that runner runs (CARGO_PKG_VERSION) —
-- the hub self-upgrades but remote workers do not, and skew must be visible.
ALTER TABLE project_coord ADD COLUMN IF NOT EXISTS version TEXT;
-- CXA-C019b: one row per bounded-context shard (the C019a ShardKind labels:
-- work, social, docs, governance, ops). `data` is the externally-tagged
-- ShardData JSON (a single-key object naming the kind) so the row is
-- self-describing and one decode path serves every kind. `revision` is that
-- shard's own write counter — per-shard observability for the
-- write-amplification this table exists to remove. A `'_legacy'` row
-- (data = the whole pre-migration aggregate) is written once by the migration
-- and frozen.
CREATE TABLE IF NOT EXISTS project_state_shard (
    project_id TEXT NOT NULL,
    shard      TEXT NOT NULL,
    revision   BIGINT NOT NULL DEFAULT 0,
    data       JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, shard)
);
-- CXA-C019b: the head-revision row — the single optimistic-concurrency token
-- per project, split out of the payload so shard writes never carry the CAS.
-- Seeded from any legacy single-blob rows on every connect. This seed is
-- deliberately lowercase: the F300 source guard pins its idempotent shape
-- (insert into ... on conflict ... do nothing) in the adapter source.
create table if not exists project_state_head (
    project_id     TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL DEFAULT 0,
    revision       BIGINT NOT NULL DEFAULT 0,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
insert into project_state_head (project_id, schema_version, revision)
    select project_id, schema_version, revision from project_state
    on conflict (project_id) do nothing;";

/// Leader lease lifetime (seconds) — a runner must renew within this or another
/// takes over. Matches the JSON store.
const LEADER_TTL_SECS: f64 = 90.0;
/// Per-ticket stage lease lifetime (seconds).
const STAGE_TTL_SECS: f64 = 1800.0;
/// How long a worker is shown online after its last heartbeat (seconds).
const WORKER_TTL_SECS: f64 = 600.0;

/// A [`StateStorePort`] storing one project aggregate per row in Postgres.
///
/// Durable state (and the transactional `claim_ticket`) live in Postgres. When a
/// [`RedisCoord`] is attached, the ephemeral leases (leader, per-stage, worker
/// registry) route to Redis instead of `project_coord` — the fast path for
/// short-lived TTL keys. Without Redis they fall back to Postgres.
pub struct SqlStateStore {
    pool: Pool,
    project_id: String,
    redis: Option<super::RedisCoord>,
    /// Optional local JSON mirror — best-effort backup written on every
    /// Postgres save so a lost Postgres volume can be re-seeded from disk
    /// (the seed logic in `app::make_store` picks it up automatically).
    local_mirror: Option<super::JsonStateStore>,
    /// Audit trail of write-backs the structural-integrity gate refused
    /// (CXA-F229). The in-memory buffer mirrors the durable `project_quarantine`
    /// table (CXA-C023) and is only the fallback when the table read fails, so
    /// the trail survives a hub restart; every refusal is also logged.
    quarantine: Arc<QuarantineLedger>,
    /// Set by [`StateStorePort::delete`]: the project was deregistered and its
    /// rows purged. A runner cycle in flight at delete time checks `STOPPED`
    /// only at the cycle boundary (runner.rs), so one of its phase-end saves
    /// can land minutes after the purge — without this flag it would silently
    /// re-INSERT the deleted row and resurrect the old team under a recreated
    /// id. A recreated project builds a FRESH store (new connect), so the flag
    /// never blocks legitimate new work. This flag is the fast in-process
    /// path; the durable `project_tombstone` row (CXA-C023) closes the
    /// cross-process and post-restart window the flag alone cannot.
    deleted: std::sync::atomic::AtomicBool,
}

impl SqlStateStore {
    /// Connect to `dsn` (a libpq/tokio-postgres URL) and ensure the schema
    /// exists, scoping this store to `project_id`.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the pool cannot be built or the schema
    /// migration fails.
    pub async fn connect(dsn: &str, project_id: impl Into<String>) -> Result<Self, PortError> {
        let pool = crate::pg::pool(dsn, "state")
            .await
            .map_err(PortError::Backend)?;
        let store = Self {
            pool,
            project_id: project_id.into(),
            redis: None,
            local_mirror: None,
            quarantine: Arc::new(QuarantineLedger::memory_only()),
            deleted: std::sync::atomic::AtomicBool::new(false),
        };
        store.migrate().await?;
        Ok(store)
    }

    /// Route the ephemeral leases (leader / stage / worker registry) through
    /// Redis at `url` instead of Postgres. Ticket claims stay transactional in
    /// Postgres. A no-op-friendly builder: pass the project id it should scope to.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the Redis URL is invalid.
    pub fn with_redis(mut self, url: &str) -> Result<Self, PortError> {
        self.redis = Some(super::RedisCoord::connect(url, self.project_id.clone())?);
        Ok(self)
    }

    /// Mirror every Postgres `save` to a local JSON file at `state_dir`. The
    /// mirror is best-effort: a write failure is logged and never fails the
    /// Postgres write. On startup the hub's seed logic re-imports this file
    /// when Postgres is empty — so a lost Postgres volume loses no work.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the directory cannot be created.
    pub fn with_local_mirror(mut self, state_dir: &std::path::Path) -> Result<Self, PortError> {
        self.local_mirror = Some(super::JsonStateStore::new(state_dir)?);
        Ok(self)
    }

    /// Best-effort write to the local JSON mirror. Never returns an error — a
    /// backup failure must not block the authoritative Postgres write.
    async fn mirror_save(&self, state: &ProjectState) {
        if let Some(mirror) = &self.local_mirror {
            if let Err(e) = mirror.save(state).await {
                tracing::warn!(
                    "[{}] local JSON mirror write failed (Postgres remains authoritative): {e}",
                    self.project_id
                );
            }
        }
    }

    /// Best-effort durable record of one F229 refusal into `project_quarantine`
    /// so the trail outlives this process. Never fails the caller — the refusal
    /// error itself still stands regardless of what happens here.
    ///
    /// A zombie writer refused on a corrupt payload AFTER a cross-process
    /// delete must not leak its refusal into the purged id's ledger: the delete
    /// transaction already purged those rows, and a recreated id would inherit
    /// the stale entry in its audit view. When in doubt (tombstone armed or
    /// unreadable, no connection) the durable write is skipped — the refusal
    /// error and the in-memory buffer still carry the trail for this instance.
    async fn persist_refusal_ledger(&self, entry: &QuarantineEntry) {
        match self.client().await {
            Ok(client) => match tombstone::exists(&client, &self.project_id).await {
                Ok(false) => {
                    if let Err(e) =
                        super::quarantine::persist_entry(&client, &self.project_id, entry).await
                    {
                        tracing::warn!(
                            "[{}] durable quarantine write failed (the refusal still stands): {e}",
                            self.project_id
                        );
                    }
                }
                Ok(true) => tracing::debug!(
                    "[{}] quarantine entry not durably recorded: project was deleted",
                    self.project_id
                ),
                Err(e) => tracing::warn!(
                    "[{}] durable quarantine write skipped, tombstone state unreadable: {e}",
                    self.project_id
                ),
            },
            Err(e) => tracing::warn!(
                "[{}] durable quarantine write skipped, no database connection: {e}",
                self.project_id
            ),
        }
    }

    async fn client(&self) -> Result<deadpool_postgres::Client, PortError> {
        self.pool
            .get()
            .await
            .map_err(|e| PortError::Backend(format!("connection: {e}")))
    }

    async fn migrate(&self) -> Result<(), PortError> {
        let mut client = self.client().await?;
        client
            .batch_execute(INIT_SQL)
            .await
            .map_err(|e| PortError::Backend(format!("migrate: {e}")))?;
        // Each unit owns its table DDL (CXA-C023); both are idempotent.
        client
            .batch_execute(tombstone::DDL)
            .await
            .map_err(|e| PortError::Backend(format!("migrate tombstone: {e}")))?;
        client
            .batch_execute(super::quarantine::DDL)
            .await
            .map_err(|e| PortError::Backend(format!("migrate quarantine: {e}")))?;
        // Connecting is the one moment an id legitimately comes back to life
        // (hub boot of a registered project, or re-registration after delete):
        // clear this id's tombstone so the recreated project can save. A
        // zombie writer never reconnects, so the per-write guard still catches
        // it (see `super::tombstone`).
        tombstone::clear(&client, &self.project_id).await?;
        // CXA-C019b: migrate a pre-shard single-blob row to shard rows on the
        // first post-upgrade connect — atomically (one tx) and concurrently
        // safe (the legacy row lock serializes migrators; the second finds
        // nothing left to do). A project that never connects is migrated
        // lazily by its next save instead (see `persist_at_revision`).
        let tx = client
            .transaction()
            .await
            .map_err(|e| PortError::Backend(format!("migrate begin: {e}")))?;
        // Box::pin: the migration future is the largest piece of connect()'s
        // future; inlined it pushed every caller of connect (app's
        // build_project) over clippy's large-future line.
        let migrated = Box::pin(Self::migrate_legacy_row_tx(&tx, &self.project_id)).await?;
        tx.commit()
            .await
            .map_err(|e| PortError::Backend(format!("migrate commit: {e}")))?;
        if migrated {
            tracing::info!(
                "[{}] migrated the legacy single-blob row to per-shard rows (CXA-C019b)",
                self.project_id
            );
        }
        Ok(())
    }

    /// Read the stored `(revision, state)` for this project, or `(0, default)`
    /// when nothing is stored yet.
    ///
    /// Shard-native projects compose the aggregate from `project_state_shard`
    /// rows joined to the head revision. A pre-C019b single-blob row (written
    /// by an older binary, or raw-present before this store ever migrated) is
    /// decoded whole — the exact pre-shard read — so a legacy project always
    /// loads correctly whether or not the migration has run yet.
    async fn load_versioned(&self) -> Result<(i64, ProjectState), PortError> {
        let client = self.client().await?;
        // One round trip over the shard-native path: every shard row joined to
        // the head revision and the legacy row (the legacy columns repeat per
        // row; the mixed-binary rule below compares revisions).
        let rows = client
            .query(
                "SELECT s.shard, s.data, h.revision, l.revision, l.data
                   FROM project_state_shard s
                   LEFT JOIN project_state_head h ON h.project_id = s.project_id
                   LEFT JOIN project_state      l ON l.project_id = s.project_id
                  WHERE s.project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("select shards: {e}")))?;
        if let Some(first) = rows.first() {
            let head_revision: i64 = first.try_get(2).map_err(|_| {
                PortError::Corrupt(
                    "shard rows exist without a head-revision row (corrupt state)".to_owned(),
                )
            })?;
            let shard_rows: Vec<(String, serde_json::Value)> = rows
                .iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, serde_json::Value>(1)))
                .collect();
            // Mixed-binary rule: an old binary writing the single-blob row
            // after this project migrated leaves a legacy row whose revision
            // is NEWER than the head — that write is the latest state, so read
            // it whole. Otherwise the shards are authoritative.
            let legacy_revision: Option<i64> = first.try_get(3).ok().flatten();
            if let Some(legacy_newer) = legacy_revision.filter(|l| *l > head_revision) {
                let value: serde_json::Value = first.get(4);
                return decode_whole_document(legacy_newer, value);
            }
            let state = compose_from_shard_rows(&shard_rows)?;
            return Ok((head_revision, state));
        }
        // No shard rows: the pre-C019b layout (or an empty project).
        let row = client
            .query_opt(
                "SELECT revision, data FROM project_state WHERE project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("select: {e}")))?;
        match row {
            None => Ok((0, ProjectState::default())),
            Some(row) => decode_whole_document(row.get(0), row.get(1)),
        }
    }

    /// Split the legacy single-blob row into shard rows inside ONE transaction
    /// (CXA-C019b). Called from `migrate()` on connect, and lazily from the
    /// persist path when a CAS misses because an unmigrated legacy row blocks
    /// the fresh head insert — a project saved by the OLD code path migrates
    /// on its next save. Crash-safe (one tx: rollback leaves the legacy row
    /// and every shard row untouched) and concurrent-safe (the legacy row is
    /// locked; a second migrator then finds it gone and skips).
    ///
    /// The original full JSON is preserved frozen as `shard = '_legacy'` for
    /// recovery/rollback tooling, the head revision moves UP to the legacy
    /// row's (never backward), and the legacy row itself is tombstoned — the
    /// shard rows are authoritative from here on.
    ///
    /// Returns whether a legacy row was actually migrated.
    async fn migrate_legacy_row_tx(
        tx: &tokio_postgres::Transaction<'_>,
        project_id: &str,
    ) -> Result<bool, PortError> {
        let Some(row) = tx
            .query_opt(
                "SELECT schema_version, revision, data FROM project_state
                  WHERE project_id = $1 FOR UPDATE",
                &[&project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("legacy row select: {e}")))?
        else {
            return Ok(false);
        };
        let legacy_revision: i64 = row.get(1);
        let value: serde_json::Value = row.get(2);
        let Ok(state) = serde_json::from_value::<ProjectState>(value.clone()) else {
            // A row this binary cannot decode is left exactly as it is:
            // load() keeps returning today's Corrupt for it, and the migration
            // must never destroy data it does not understand.
            tracing::warn!("[{project_id}] legacy row not decodable; left unmigrated");
            return Ok(false);
        };
        if state.schema_version > SCHEMA_VERSION {
            // Newer than this binary understands — leave it for a newer one;
            // load() keeps returning today's "newer than supported" Corrupt.
            tracing::warn!(
                "[{project_id}] legacy row schema_version {} newer than supported \
                 {SCHEMA_VERSION}; left unmigrated",
                state.schema_version
            );
            return Ok(false);
        }
        let legacy_schema_version = state.schema_version;
        let sharded = state.into_shards();
        for (label, wrapped) in shard_row_payloads(&sharded)? {
            tx.execute(
                "INSERT INTO project_state_shard (project_id, shard, revision, data)
                 VALUES ($1, $2, 1, $3)
                 ON CONFLICT (project_id, shard) DO UPDATE
                     SET data = EXCLUDED.data, updated_at = now()",
                &[&project_id, &label, &wrapped],
            )
            .await
            .map_err(|e| PortError::Backend(format!("shard insert: {e}")))?;
        }
        // Freeze the original document for recovery/rollback tooling. The row
        // is written once: ON CONFLICT keeps the FIRST frozen copy if a legacy
        // row ever reappears (an old binary's rewrite) and migrates again.
        tx.execute(
            "INSERT INTO project_state_shard (project_id, shard, revision, data)
             VALUES ($1, '_legacy', $2, $3)
             ON CONFLICT (project_id, shard) DO NOTHING",
            &[&project_id, &legacy_revision, &value],
        )
        .await
        .map_err(|e| PortError::Backend(format!("legacy freeze: {e}")))?;
        // The head revision moves UP to the legacy row's — an old binary may
        // have bumped the single-blob row after the head was seeded, and the
        // single-blob row was the truth until this very transaction.
        tx.execute(
            "INSERT INTO project_state_head (project_id, schema_version, revision)
             VALUES ($1, $2, $3)
             ON CONFLICT (project_id) DO UPDATE
                 SET revision = EXCLUDED.revision, updated_at = now()
               WHERE project_state_head.revision < EXCLUDED.revision",
            &[
                &project_id,
                &i32::try_from(legacy_schema_version).unwrap_or(i32::MAX),
                &legacy_revision,
            ],
        )
        .await
        .map_err(|e| PortError::Backend(format!("head sync: {e}")))?;
        tx.execute(
            "DELETE FROM project_state WHERE project_id = $1",
            &[&project_id],
        )
        .await
        .map_err(|e| PortError::Backend(format!("legacy tombstone: {e}")))?;
        Ok(true)
    }
}

/// The stored label of a shard kind — [`ShardKind`]'s own serde vocabulary
/// (`work`, `social`, …), the same one the C019a seam and the F300 guards use.
fn shard_label(kind: ShardKind) -> String {
    serde_json::to_string(&kind)
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned()
}

/// One shard row's `(label, externally-tagged payload)` pair — the write unit
/// of the sharded layout. The payload wraps the kind's struct as
/// `{"work": {...}}` so a row is self-describing and one decode path serves
/// every kind.
///
/// # Errors
/// [`PortError::Backend`] if a shard payload fails to serialize — a payload
/// that cannot be encoded must fail the save, never fall back to a Null row.
fn shard_row_payloads(
    sharded: &ShardedState,
) -> Result<Vec<(String, serde_json::Value)>, PortError> {
    ShardKind::ALL
        .iter()
        .map(|kind| {
            let data = match kind {
                ShardKind::Work => ShardData::Work(sharded.work.clone()),
                ShardKind::Social => ShardData::Social(sharded.social.clone()),
                ShardKind::Docs => ShardData::Docs(sharded.docs.clone()),
                ShardKind::Governance => ShardData::Governance(sharded.governance.clone()),
                ShardKind::Ops => ShardData::Ops(sharded.ops.clone()),
            };
            let wrapped = serde_json::to_value(&data)
                .map_err(|e| PortError::Backend(format!("encode shard: {e}")))?;
            Ok((shard_label(*kind), wrapped))
        })
        .collect()
}

/// Reassemble the aggregate from `(label, payload)` shard rows. Unknown labels
/// (the frozen `'_legacy'` row, future kinds) are ignored by the join; a
/// missing shard contributes its serde-default payload, so a partial row set
/// still reassembles a state the stores accept. A row whose payload disagrees
/// with its label is corrupt — refusing beats silently trusting either.
fn compose_from_shard_rows(
    shard_rows: &[(String, serde_json::Value)],
) -> Result<ProjectState, PortError> {
    let mut sharded = ShardedState::default();
    for (label, value) in shard_rows {
        let Some(kind) = ShardKind::ALL
            .iter()
            .copied()
            .find(|k| shard_label(*k) == *label)
        else {
            continue;
        };
        let data: ShardData = serde_json::from_value(value.clone())
            .map_err(|e| PortError::Corrupt(format!("shard {label} not decodable: {e}")))?;
        if data.kind() != kind {
            return Err(PortError::Corrupt(format!(
                "shard row labelled '{label}' carries a '{}' payload",
                shard_label(data.kind())
            )));
        }
        match data {
            ShardData::Work(work) => sharded.work = work,
            ShardData::Social(social) => sharded.social = social,
            ShardData::Docs(docs) => sharded.docs = docs,
            ShardData::Governance(governance) => sharded.governance = governance,
            ShardData::Ops(ops) => sharded.ops = ops,
        }
    }
    decode_checked(ProjectState::from_shards(sharded))
}

/// Decode a pre-C019b whole-document row — the exact pre-shard read path,
/// including the schema-version ceiling.
fn decode_whole_document(
    revision: i64,
    value: serde_json::Value,
) -> Result<(i64, ProjectState), PortError> {
    let state: ProjectState = serde_json::from_value(value)
        .map_err(|e| PortError::Corrupt(format!("row not decodable: {e}")))?;
    decode_checked(state).map(|state| (revision, state))
}

/// The schema-version ceiling every decode path applies (semantics unchanged
/// since the first SQL adapter).
fn decode_checked(state: ProjectState) -> Result<ProjectState, PortError> {
    if state.schema_version > SCHEMA_VERSION {
        return Err(PortError::Corrupt(format!(
            "state schema_version {} newer than supported {SCHEMA_VERSION}",
            state.schema_version
        )));
    }
    Ok(state)
}

#[async_trait]
impl StateStorePort for SqlStateStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        self.load_versioned().await.map(|(_, state)| state)
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        // Legacy path kept intact for older callers who never captured a version.
        self.persist_at_revision(state.clone(), None).await
    }

    async fn save_expecting(
        &self,
        state: &ProjectState,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        // Cross-process optimistic concurrency carried over REST/gateway calls:
        // use exactly what THIS caller loaded rather than re-reading at write time.
        self.persist_at_revision(state.clone(), expected_revision)
            .await
    }

    async fn current_version(&self) -> Result<Option<i64>, PortError> {
        let client = self.client().await?;
        let row = client
            .query_opt(
                "SELECT revision FROM project_state_head WHERE project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("select rev: {e}")))?;
        if let Some(row) = row {
            return Ok(Some(row.get::<_, i64>(0)));
        }
        // No head row yet: an unmigrated legacy single-blob row's revision is
        // still the truth (the migration syncs it into the head when it runs),
        // and a project never written exposes the baseline revision 0.
        let legacy = client
            .query_opt(
                "SELECT revision FROM project_state WHERE project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("select legacy rev: {e}")))?;
        // An absent row has not been written yet -> baseline revision 0 matches
        // [`Self::persist_at_revision`]'s first insert (`revision = 1`).
        Ok(Some(legacy.map_or(0_i64, |r| r.get::<_, i64>(0))))
    }

    async fn claim_ticket(
        &self,
        id: &TicketId,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
        // A deleted project's store refuses all state writes (see `deleted`).
        // `false` = "you did not win" — the honest answer for a late claim.
        if self.deleted.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(false);
        }
        // Serialize claims cluster-wide inside one transaction. Two ordering
        // rules make this both narrow and deadlock-free:
        // 1. The head row is locked FIRST (the revision bump below takes its
        //    row lock) — the same lock order every writer uses, so a claim and
        //    a shard-diff save never wait on each other in a cycle. The bump
        //    is transactional: a losing claim below rolls it back.
        // 2. Only the Tickets shard row is then locked FOR UPDATE — the
        //    whole-aggregate row lock this method used to take is exactly the
        //    write contention CXA-C019b removes; claims no longer serialize
        //    behind unrelated shard writes.
        let work_label = shard_label(ShardKind::Work);
        let mut client = self.client().await?;
        let tx = client
            .transaction()
            .await
            .map_err(|e| PortError::Backend(format!("begin: {e}")))?;
        let bumped = tx
            .execute(
                "UPDATE project_state_head
                    SET revision = revision + 1, updated_at = now()
                  WHERE project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("bump head: {e}")))?;
        if bumped == 0 {
            // Never written (or purged by a delete) — nothing to claim, and a
            // late claim must not recreate the row.
            return Ok(false);
        }
        let Some(row) = tx
            .query_opt(
                "SELECT data FROM project_state_shard
                  WHERE project_id = $1 AND shard = $2
                  FOR UPDATE OF project_state_shard",
                &[&self.project_id, &work_label],
            )
            .await
            .map_err(|e| PortError::Backend(format!("select for update: {e}")))?
        else {
            return Ok(false);
        };
        let value: serde_json::Value = row.get(0);
        let shard: ShardData = serde_json::from_value(value)
            .map_err(|e| PortError::Corrupt(format!("work shard not decodable: {e}")))?;
        let ShardData::Work(mut work) = shard else {
            return Err(PortError::Corrupt(format!(
                "shard row '{work_label}' carries a '{}' payload",
                shard_label(shard.kind())
            )));
        };
        let Some(ticket) = work.tickets.iter_mut().find(|t| t.id() == id) else {
            return Ok(false);
        };
        if ticket.claimed_by().is_some() || ticket.claim(Role::System, worker, now).is_err() {
            return Ok(false);
        }
        let updated = serde_json::to_value(ShardData::Work(work))
            .map_err(|e| PortError::Backend(format!("encode: {e}")))?;
        tx.execute(
            "UPDATE project_state_shard
                SET data = $3, revision = revision + 1, updated_at = now()
              WHERE project_id = $1 AND shard = $2",
            &[&self.project_id, &work_label, &updated],
        )
        .await
        .map_err(|e| PortError::Backend(format!("update: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| PortError::Backend(format!("commit: {e}")))?;
        // The local JSON mirror stays a whole-state backup file (format
        // unchanged), so the claim still mirrors — one extra read, the same
        // whole-aggregate read this claim has always paid.
        match self.load().await {
            Ok(whole) => self.mirror_save(&whole).await,
            Err(e) => tracing::warn!(
                "[{}] post-claim mirror read failed (the claim itself committed): {e}",
                self.project_id
            ),
        }
        Ok(true)
    }

    async fn acquire_leader(&self, worker: &str, _now: &str) -> Result<bool, PortError> {
        if let Some(r) = &self.redis {
            return r.acquire_leader(worker).await;
        }
        self.upsert_lease("leader", "", worker, LEADER_TTL_SECS)
            .await
    }

    async fn claim_stage(
        &self,
        id: &TicketId,
        stage: &str,
        worker: &str,
        _now: &str,
    ) -> Result<bool, PortError> {
        if let Some(r) = &self.redis {
            return r.claim_stage(&id.to_string(), stage, worker).await;
        }
        let key = format!("{id}|{stage}");
        self.upsert_lease("stage", &key, worker, STAGE_TTL_SECS)
            .await
    }

    async fn heartbeat_worker(
        &self,
        worker: &str,
        role: &str,
        ticket: &str,
        caps: &WorkerCaps,
        now: &str,
    ) -> Result<(), PortError> {
        if let Some(r) = &self.redis {
            return r.heartbeat_worker(worker, role, ticket, caps, now).await;
        }
        let client = self.client().await?;
        let engines_csv = caps.engines.join(",");
        let models_csv = caps.models.join("\n");
        client
            .execute(
                "INSERT INTO project_coord
                    (project_id, kind, coord_key, worker, at, role, ticket,
                     engines, models, gitcheck, tooling, version)
                 VALUES ($1, 'worker', $2, $2, now(), $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT (project_id, kind, coord_key) DO UPDATE
                    SET at = now(), role = EXCLUDED.role, ticket = EXCLUDED.ticket,
                        engines = EXCLUDED.engines, models = EXCLUDED.models,
                        gitcheck = EXCLUDED.gitcheck, tooling = EXCLUDED.tooling,
                        version = EXCLUDED.version",
                &[
                    &self.project_id,
                    &worker,
                    &role,
                    &ticket,
                    &engines_csv,
                    &models_csv,
                    &caps
                        .git
                        .as_ref()
                        .and_then(|g| serde_json::to_string(g).ok())
                        .unwrap_or_default(),
                    &caps
                        .tooling
                        .as_ref()
                        .and_then(|t| serde_json::to_string(t).ok())
                        .unwrap_or_default(),
                    &caps.version,
                ],
            )
            .await
            .map_err(|e| PortError::Backend(format!("heartbeat: {e}")))?;
        Ok(())
    }

    async fn workers(&self) -> Result<Vec<WorkerEntry>, PortError> {
        if let Some(r) = &self.redis {
            return r.workers().await;
        }
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT worker, coalesce(role,''), coalesce(ticket,''),
                        to_char(at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),
                        coalesce(engines,''), coalesce(models,''),
                        coalesce(gitcheck,''), coalesce(tooling,''),
                        coalesce(version,'')
                   FROM project_coord
                  WHERE project_id = $1 AND kind = 'worker'
                    AND at > now() - make_interval(secs => $2)
                  ORDER BY worker",
                &[&self.project_id, &WORKER_TTL_SECS],
            )
            .await
            .map_err(|e| PortError::Backend(format!("workers: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| WorkerEntry {
                worker: r.get(0),
                role: r.get(1),
                ticket: r.get(2),
                at: r.get(3),
                engines: r
                    .get::<_, String>(4)
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                    .collect(),
                models: r
                    .get::<_, String>(5)
                    .lines()
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                    .collect(),
                git: serde_json::from_str(&r.get::<_, String>(6)).ok(),
                tooling: serde_json::from_str(&r.get::<_, String>(7)).ok(),
                version: r.get(8),
            })
            .collect())
    }

    async fn set_desired(&self, operator: &str, running: bool) -> Result<(), PortError> {
        if let Some(r) = &self.redis {
            return r.set_desired(operator, running).await;
        }
        // Postgres-only fallback: persist to project_coord table with kind='desired'.
        let client = self
            .client()
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?;
        client
            .execute(
                "INSERT INTO project_coord (project_id, kind, coord_key, worker, at)
                 VALUES ($1, 'desired', $2, $3, NOW())
                 ON CONFLICT (project_id, kind, coord_key) DO UPDATE SET worker = $3, at = NOW()",
                &[
                    &self.project_id,
                    &format!("op:{operator}"),
                    &running.to_string(),
                ],
            )
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn get_desired(&self, operator: &str) -> Result<Option<bool>, PortError> {
        if let Some(r) = &self.redis {
            return r.get_desired(operator).await;
        }
        // Postgres-only fallback.
        let client = self
            .client()
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?;
        let row = client
            .query_opt(
                "SELECT worker FROM project_coord WHERE project_id = $1 AND kind = 'desired' AND coord_key = $2",
                &[&self.project_id, &format!("op:{operator}")],
            )
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?;
        Ok(row.and_then(|r| r.get::<_, String>(0).parse::<bool>().ok()))
    }

    async fn acquire_operator(&self, operator: &str, instance: &str) -> Result<bool, PortError> {
        if let Some(r) = &self.redis {
            return r.acquire_operator_lock(operator, instance).await;
        }
        Ok(true)
    }

    async fn quarantined(&self) -> Vec<QuarantineEntry> {
        // The durable `project_quarantine` table is the ledger: a fresh store
        // instance (or a hub restarted after the refusal) reads the same trail.
        match self.client().await {
            Ok(client) => match super::quarantine::load_recent(&client, &self.project_id).await {
                Ok(entries) => return entries,
                Err(e) => tracing::warn!(
                    "[{}] durable quarantine read failed, falling back to the \
                     in-memory buffer: {e}",
                    self.project_id
                ),
            },
            Err(e) => tracing::warn!(
                "[{}] no database connection for the quarantine read, falling \
                 back to the in-memory buffer: {e}",
                self.project_id
            ),
        }
        self.quarantine.recent()
    }

    /// Purge this project's row in `project_state` AND every `project_coord`
    /// row scoped to the same id (CXA-B130): one transaction, so a shared
    /// store never keeps a half-purged project. The row was the resurrection
    /// bug — recreating a project under a deleted id adopted the stale
    /// aggregate deterministically (CXA-B126's "workspace already has
    /// tickets" 500). Coordination rows go too: the operator's desired-run
    /// state is persistent and would auto-resume the deleted project's
    /// runner under the reused id.
    ///
    /// The SAME transaction arms the durable delete tombstone (CXA-C023) and
    /// purges this project's quarantine rows: the tombstone is what makes the
    /// purge hold against writers in other processes (or after a restart) that
    /// the in-process `deleted` flag cannot see — every guarded write filters
    /// on its absence, so a late phase-end save is refused at the database
    /// instead of re-INSERTing the purged row. The ledger rows go too: a
    /// deleted project's refusal trail must not leak onto a recreated id's
    /// audit view.
    async fn delete(&self) -> Result<(), PortError> {
        // Arm the write refusal BEFORE purging, so a save already in flight
        // when the rows go finds the flag (see `deleted` for the race this
        // narrows).
        self.deleted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut client = self.client().await?;
        let tx = client
            .transaction()
            .await
            .map_err(|e| PortError::Backend(format!("begin: {e}")))?;
        tombstone::arm(&tx, &self.project_id, "delete()").await?;
        let state_rows = tx
            .execute(
                "DELETE FROM project_state WHERE project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("delete state: {e}")))?;
        // The CXA-C019b footprint goes with it: shard rows and the head
        // revision row. A recreated id must start truly fresh — a surviving
        // shard row or head revision would resurrect the deleted team's data
        // (or its revision counter) under the reused id.
        let shard_rows = tx
            .execute(
                "DELETE FROM project_state_shard WHERE project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("delete shards: {e}")))?;
        tx.execute(
            "DELETE FROM project_state_head WHERE project_id = $1",
            &[&self.project_id],
        )
        .await
        .map_err(|e| PortError::Backend(format!("delete head: {e}")))?;
        tx.execute(
            "DELETE FROM project_coord WHERE project_id = $1",
            &[&self.project_id],
        )
        .await
        .map_err(|e| PortError::Backend(format!("delete coord: {e}")))?;
        super::quarantine::purge_project(&tx, &self.project_id).await?;
        tx.commit()
            .await
            .map_err(|e| PortError::Backend(format!("commit: {e}")))?;
        // The ephemeral Redis keys (leases, presence) are TTL'd, but the
        // desired-run state is persistent — sweep the project's whole
        // keyspace. Best-effort: a Redis outage must not make the hub re-report
        // an already-purged project as undeleteable.
        if let Some(r) = &self.redis {
            if let Err(e) = r.forget_project().await {
                tracing::warn!(
                    "[{}] redis keyspace cleanup failed (leases expire on their own): {e}",
                    self.project_id
                );
            }
        }
        // Drop the local JSON mirror too, or the seed logic in `make_store`
        // would re-import it into the recreated project's empty Postgres row.
        if let Some(mirror) = &self.local_mirror {
            if let Err(e) = mirror.delete().await {
                tracing::warn!(
                    "[{}] local JSON mirror cleanup failed (best-effort backup): {e}",
                    self.project_id
                );
            }
        }
        tracing::info!(
            "[{}] deleted persisted state and armed the tombstone ({state_rows} state row(s), \
             {shard_rows} shard row(s) purged)",
            self.project_id
        );
        Ok(())
    }
}

impl SqlStateStore {
    /// Atomically take or renew a coordination lease. Wins (`true`) when the row
    /// is absent, already ours, or its lease has expired — all decided inside one
    /// conditional UPSERT so concurrent machines agree on a single holder.
    async fn upsert_lease(
        &self,
        kind: &str,
        key: &str,
        worker: &str,
        ttl_secs: f64,
    ) -> Result<bool, PortError> {
        let client = self.client().await?;
        let row = client
            .query_opt(
                "INSERT INTO project_coord (project_id, kind, coord_key, worker, at)
                 VALUES ($1, $2, $3, $4, now())
                 ON CONFLICT (project_id, kind, coord_key) DO UPDATE
                    SET worker = EXCLUDED.worker, at = now()
                    WHERE project_coord.worker = $4
                       OR project_coord.at < now() - make_interval(secs => $5)
                 RETURNING worker",
                &[&self.project_id, &kind, &key, &worker, &ttl_secs],
            )
            .await
            .map_err(|e| PortError::Backend(format!("lease upsert: {e}")))?;
        Ok(row.is_some_and(|r| r.get::<_, String>(0) == worker))
    }

    /// Win the head revision for a save: run the guarded CAS inside `tx`, and
    /// on a miss disambiguate — a delete tombstone refuses; an unmigrated
    /// legacy single-blob row is migrated in the same transaction (freezing
    /// the original document and syncing the head revision up to it) and the
    /// CAS retried once; anything else is the ordinary stale-writer conflict.
    ///
    /// # Errors
    /// [`tombstone::refusal`] for a tombstoned project, [`PortError::Conflict`]
    /// for a stale expected revision, [`PortError::Backend`] on statement
    /// failures.
    async fn win_head_revision(
        tx: &tokio_postgres::Transaction<'_>,
        project_id: &str,
        schema_version: u32,
        expected: i64,
    ) -> Result<(), PortError> {
        let cas = (i32::try_from(schema_version).unwrap_or(i32::MAX), expected);
        let rows = tx
            .execute(
                tombstone::GUARDED_HEAD_CAS_SQL,
                &[&project_id, &cas.0, &cas.1],
            )
            .await
            .map_err(|e| PortError::Backend(format!("head cas: {e}")))?;
        if rows > 0 {
            return Ok(());
        }
        // Either a concurrent writer moved the revision, a delete tombstoned
        // this project in another process, or an unmigrated legacy single-blob
        // row blocked the fresh head insert. A fresh read (the same statement
        // snapshot the miss was decided in) tells the three apart.
        if tombstone::exists_tx(tx, project_id).await? {
            return Err(tombstone::refusal(project_id));
        }
        if Self::migrate_legacy_row_tx(tx, project_id).await? {
            let retried = tx
                .execute(
                    tombstone::GUARDED_HEAD_CAS_SQL,
                    &[&project_id, &cas.0, &cas.1],
                )
                .await
                .map_err(|e| PortError::Backend(format!("head cas retry: {e}")))?;
            if retried > 0 {
                return Ok(());
            }
        }
        Err(PortError::Conflict(
            "state changed since last read (concurrent writer)".to_owned(),
        ))
    }

    /// Validate, then CAS-persist one snapshot against a caller-chosen expected
    /// revision — as a head-revision win plus a changed-shard diff write
    /// (CXA-C019b).
    ///
    /// Optimistic concurrency: the head revision in `project_state_head` is
    /// won first via [`tombstone::GUARDED_HEAD_CAS_SQL`] — insert when absent,
    /// otherwise bump only if it has not moved past what this writer expected.
    /// When `expected_revision` is supplied it is used directly as the
    /// predicate — two writers racing across process boundaries both check
    /// against what THEY each loaded, so a stale writer affects zero rows and
    /// gets a [`PortError::Conflict`]. When `None`, fall back to re-reading at
    /// write time (the historical default, sound for intra-process writers
    /// sharing one store instance).
    ///
    /// Only after the head is won are the shard rows written — and only the
    /// shards whose serialized payload actually changed (the write
    /// amplification this layout removes). The whole write is ONE transaction,
    /// so a crash between the CAS and the shard writes leaves the previous
    /// consistent snapshot; the loser of the CAS writes nothing at all.
    ///
    /// A CAS that misses because an UNMIGRATED legacy single-blob row blocks
    /// the fresh head insert migrates that row in the same transaction
    /// ([`Self::migrate_legacy_row_tx`], which also freezes the original
    /// document as `'_legacy'` and syncs the head revision up to it) and
    /// retries the CAS once — a project saved by the old code path migrates on
    /// its next save. Any legacy row surviving a WINNING save is stale
    /// residue (the shards are authoritative from here) and is tombstoned.
    async fn persist_at_revision(
        &self,
        state: ProjectState,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        // A deleted project's store refuses all state writes (see `deleted`) —
        // an explicit error, never a silent "saved", so the stopping runner's
        // cycle reports the refusal instead of believing it persisted. The
        // durable tombstone re-check below covers every other process.
        if self.deleted.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(tombstone::refusal(&self.project_id));
        }
        // The pre-existing schema-level validation is untouched; the
        // structural-integrity audit (CXA-F229) is the additional gate. The
        // recorded refusal is persisted into `project_quarantine`
        // (best-effort — it must not mask the refusal error itself), so the
        // trail outlives this process. The gate runs BEFORE any row write: a
        // refused payload reaches no legacy row and no shard row.
        let mut state = state;
        if let Err(refused) = gate_save(&mut state, &self.quarantine) {
            tracing::error!(
                "[{}] write-back refused by structural integrity audit: {}",
                self.project_id,
                refused.error
            );
            if let Some(entry) = refused.quarantined {
                self.persist_refusal_ledger(&entry).await;
            }
            return Err(refused.error);
        }
        // Decompose the validated snapshot into its bounded-context shards;
        // the diff below compares serialized payloads, so an unchanged shard
        // is never rewritten.
        let sharded = state.into_shards();
        let schema_version = sharded.work.schema_version;

        let mut client = self.client().await?;
        let expected = match expected_revision {
            Some(rev) => rev,
            None => self.load_versioned().await?.0,
        };
        let tx = client
            .transaction()
            .await
            .map_err(|e| PortError::Backend(format!("begin: {e}")))?;
        Self::win_head_revision(&tx, &self.project_id, schema_version, expected).await?;
        // Won the head at `expected + 1`: write only what changed. The stored
        // payloads are read inside the same transaction, so the diff is
        // consistent with the revision this writer just claimed.
        let stored = tx
            .query(
                "SELECT shard, data FROM project_state_shard WHERE project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("select shards: {e}")))?;
        let stored: std::collections::BTreeMap<String, serde_json::Value> = stored
            .into_iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, serde_json::Value>(1)))
            .collect();
        for (label, wrapped) in shard_row_payloads(&sharded)? {
            if stored
                .get(&label)
                .is_some_and(|current| *current == wrapped)
            {
                continue;
            }
            tx.execute(
                "INSERT INTO project_state_shard (project_id, shard, revision, data)
                 VALUES ($1, $2, 1, $3)
                 ON CONFLICT (project_id, shard) DO UPDATE
                     SET data = EXCLUDED.data,
                         revision = project_state_shard.revision + 1,
                         updated_at = now()",
                &[&self.project_id, &label, &wrapped],
            )
            .await
            .map_err(|e| PortError::Backend(format!("shard upsert: {e}")))?;
        }
        // A surviving single-blob row is stale residue once the shards are
        // authoritative: this save carried the caller's complete loaded state,
        // so the legacy document is superseded by definition (also closes the
        // old-binary-rewrote-the-legacy-row window).
        tx.execute(
            "DELETE FROM project_state WHERE project_id = $1",
            &[&self.project_id],
        )
        .await
        .map_err(|e| PortError::Backend(format!("legacy residue: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| PortError::Backend(format!("commit: {e}")))?;
        // The local JSON mirror stays a whole-state backup file (format
        // unchanged, so seed/restore keeps working) — reassembled from the
        // exact shards that just committed.
        self.mirror_save(&ProjectState::from_shards(sharded)).await;
        Ok(())
    }
}
