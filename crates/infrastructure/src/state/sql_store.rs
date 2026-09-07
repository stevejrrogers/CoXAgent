//! `SqlStateStore` — a Postgres-backed [`StateStorePort`] for multi-tenant
//! deployments. Each project's aggregate is one JSONB row keyed by project id,
//! so many projects share one database while staying isolated by key.
//!
//! This is the port swap the architecture promised: use cases are unchanged;
//! only the adapter differs from [`super::JsonStateStore`]. Optimistic
//! concurrency (a monotonic `revision`) rejects lost updates from two writers.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{
    QuarantineEntry, StateStorePort, WorkerCaps, WorkerEntry,
};
use coxagent_application::state::{
    ProjectState, SCHEMA_VERSION, ShardKind, StateShard,
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
-- CXA-C019b: per-shard JSONB columns, the native home of each bounded
-- context's payload (see state::shards in the application crate and
-- super::sql_shards). Nullable by design: rows written by a pre-shard
-- writer leave them NULL, and a shard read of a NULL column falls back to
-- projecting the legacy `data` envelope, which stays the rollback path.
-- Every writer through this adapter dual-writes envelope + columns in one
-- statement (including the JSON-mirror seed, which goes through save()), so
-- the two never disagree.
ALTER TABLE project_state ADD COLUMN IF NOT EXISTS shard_work JSONB;
ALTER TABLE project_state ADD COLUMN IF NOT EXISTS shard_social JSONB;
ALTER TABLE project_state ADD COLUMN IF NOT EXISTS shard_docs JSONB;
ALTER TABLE project_state ADD COLUMN IF NOT EXISTS shard_governance JSONB;
ALTER TABLE project_state ADD COLUMN IF NOT EXISTS shard_ops JSONB;";

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

    async fn client(&self) -> Result<deadpool_postgres::Client, PortError> {
        self.pool
            .get()
            .await
            .map_err(|e| PortError::Backend(format!("connection: {e}")))
    }

    async fn migrate(&self) -> Result<(), PortError> {
        let client = self.client().await?;
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
        Ok(())
    }

    /// Read the stored `(revision, state)` for this project, or `(0, default)`
    /// when the row does not exist yet.
    async fn load_versioned(&self) -> Result<(i64, ProjectState), PortError> {
        let client = self.client().await?;
        let row = client
            .query_opt(
                "SELECT revision, data FROM project_state WHERE project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("select: {e}")))?;
        match row {
            None => Ok((0, ProjectState::default())),
            Some(row) => {
                let revision: i64 = row.get(0);
                let value: serde_json::Value = row.get(1);
                let state: ProjectState = serde_json::from_value(value)
                    .map_err(|e| PortError::Corrupt(format!("row not decodable: {e}")))?;
                if state.schema_version > SCHEMA_VERSION {
                    return Err(PortError::Corrupt(format!(
                        "state schema_version {} newer than supported {SCHEMA_VERSION}",
                        state.schema_version
                    )));
                }
                Ok((revision, state))
            }
        }
    }
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
                "SELECT revision FROM project_state WHERE project_id = $1",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("select rev: {e}")))?;
        // An absent row has not been written yet -> baseline revision 0 matches
        // [`Self::persist_at_revision`]'s first insert (`revision = 1`).
        Ok(Some(row.map_or(0_i64, |r| r.get::<_, i64>(0))))
    }

    /// Native shard read (CXA-C019b): ONE column of the row, never the whole
    /// document. A row written by a pre-shard writer leaves the columns NULL;
    /// those fall back to projecting the legacy `data` envelope, which is
    /// also the rollback path, so the columns can never be the only copy of
    /// a field.
    ///
    /// # Errors
    /// [`PortError`] on a read or decode failure.
    async fn load_shard(&self, kind: ShardKind) -> Result<StateShard, PortError> {
        // The connection is scoped to the column read: the envelope fallback
        // below takes its own client from the pool rather than sharing this
        // call's slot.
        let payload: Option<serde_json::Value> = {
            let client = self.client().await?;
            let sql = format!(
                "SELECT {} FROM project_state WHERE project_id = $1",
                super::sql_shards::shard_column(kind)
            );
            let row = client
                .query_opt(sql.as_str(), &[&self.project_id])
                .await
                .map_err(|e| PortError::Backend(format!("select shard: {e}")))?;
            match row {
                // Never-written project: a fresh slice of the aggregate — the
                // same state a full load would serve.
                None => return Ok(super::sql_shards::default_shard(kind)),
                Some(row) => row.get(0),
            }
        };
        match payload {
            Some(value) => super::sql_shards::decode_shard(kind, value),
            None => Ok(self.load().await?.shard(kind)),
        }
    }

    /// Merge ONE shard into the persisted aggregate (see
    /// [`StateStorePort::save_shard`]): a native dual-write of the merged
    /// aggregate, not the default's load-merge-save round trip through
    /// [`Self::save`].
    ///
    /// # Errors
    /// [`PortError`] — including [`PortError::Conflict`] when a concurrent
    /// writer commits between this call's read and write — on a load, save or
    /// validation failure.
    async fn save_shard(&self, shard: &StateShard) -> Result<(), PortError> {
        self.save_shard_expecting(shard, None).await
    }

    /// [`Self::save_shard`] with optimistic concurrency: the merged aggregate
    /// commits only if the row's revision has not moved past what this caller
    /// supplied — or, when no revision was supplied, past the revision this
    /// call itself read as the merge base (the CAS token and the merge base
    /// come from ONE `load_versioned` read, so a concurrent writer between
    /// read and write is a conflict, never a silent clobber).
    ///
    /// # Errors
    /// [`PortError`] — including [`PortError::Conflict`] on a stale revision —
    /// on a load, save or validation failure.
    async fn save_shard_expecting(
        &self,
        shard: &StateShard,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        let (revision, mut state) = self.load_versioned().await?;
        state.with_shard(shard.clone());
        self.persist_at_revision(state, Some(expected_revision.unwrap_or(revision)))
            .await
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
        // Serialize claims cluster-wide with a row lock: the whole
        // read-check-set-write runs in one transaction, so two machines racing
        // on the same backlog can never both win the ticket.
        let mut client = self.client().await?;
        let tx = client
            .transaction()
            .await
            .map_err(|e| PortError::Backend(format!("begin: {e}")))?;
        let Some(row) = tx
            .query_opt(
                "SELECT data FROM project_state WHERE project_id = $1 FOR UPDATE",
                &[&self.project_id],
            )
            .await
            .map_err(|e| PortError::Backend(format!("select for update: {e}")))?
        else {
            return Ok(false);
        };
        let value: serde_json::Value = row.get(0);
        let mut state: ProjectState = serde_json::from_value(value)
            .map_err(|e| PortError::Corrupt(format!("row not decodable: {e}")))?;
        let Some(ticket) = state.ticket_mut(id) else {
            return Ok(false);
        };
        if ticket.claimed_by().is_some() || ticket.claim(Role::System, worker, now).is_err() {
            return Ok(false);
        }
        let newval =
            serde_json::to_value(&state).map_err(|e| PortError::Backend(format!("encode: {e}")))?;
        // Dual-write (CXA-C019b): the claim mutates tickets — Work-shard
        // fields — so the same statement must refresh the shard columns, or a
        // native `load_shard(Work)` would serve the pre-claim backlog while
        // the envelope showed the claim. One statement keeps both
        // representations coherent with the commit.
        let shard_doc = super::sql_shards::shard_doc(&state)?;
        tx.execute(
            "UPDATE project_state
                SET data = $1,
                    shard_work = $3::jsonb->'work', shard_social = $3::jsonb->'social',
                    shard_docs = $3::jsonb->'docs', shard_governance = $3::jsonb->'governance',
                    shard_ops = $3::jsonb->'ops',
                    revision = revision + 1, updated_at = now()
              WHERE project_id = $2",
            &[&newval, &self.project_id, &shard_doc],
        )
        .await
        .map_err(|e| PortError::Backend(format!("update: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| PortError::Backend(format!("commit: {e}")))?;
        self.mirror_save(&state).await;
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
            "[{}] deleted persisted state and armed the tombstone ({state_rows} state row(s) purged)",
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

    /// Validate, encode and CAS-persist one snapshot against a caller-chosen
    /// expected revision.
    ///
    /// Optimistic concurrency: insert when absent, otherwise bump the revision
    /// only if it has not moved past what this writer expected. When
    /// `expected_revision` is supplied it is used directly as the predicate — two
    /// writers racing across process boundaries both check against what THEY each
    /// loaded, so a stale writer affects zero rows and gets a [`PortError::Conflict`].
    /// When `None`, fall back to re-reading at write time (the historical default,
    /// sound for intra-process writers sharing one store instance).
    ///
    /// The whole write is ONE guarded statement ([`tombstone::GUARDED_SAVE_SQL`]):
    /// both the insert and the conflict-update branch filter on the delete
    /// tombstone's absence (CXA-C023), so "was this project deleted?" and "is my
    /// revision current?" are decided atomically and a zombie writer in any
    /// process is refused the moment a concurrent delete commits. Zero rows
    /// affected is disambiguated by a fresh tombstone re-check: refusal if armed,
    /// the ordinary conflict otherwise.
    async fn persist_at_revision(
        &self,
        mut state: ProjectState,
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
        // trail outlives this process.
        if let Err(refused) = gate_save(&mut state, &self.quarantine) {
            tracing::error!(
                "[{}] write-back refused by structural integrity audit: {}",
                self.project_id,
                refused.error
            );
            if let Some(entry) = refused.quarantined {
                // A zombie writer refused on a corrupt payload AFTER a
                // cross-process delete must not leak its refusal into the
                // purged id's ledger: the delete transaction already purged
                // those rows, and a recreated id would inherit the stale
                // entry in its audit view. When in doubt (tombstone armed or
                // unreadable, no connection) skip the durable write — the
                // refusal error and the in-memory buffer still carry the
                // trail for this instance.
                match self.client().await {
                    Ok(client) => match tombstone::exists(&client, &self.project_id).await {
                        Ok(false) => {
                            if let Err(e) = super::quarantine::persist_entry(
                                &client,
                                &self.project_id,
                                &entry,
                            )
                            .await
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
            return Err(refused.error);
        }
        let value =
            serde_json::to_value(&state).map_err(|e| PortError::Backend(format!("encode: {e}")))?;
        // Dual-write (CXA-C019b): the same committed write refreshes the
        // per-shard columns from the sharded view of the very snapshot being
        // persisted, so envelope and shard readers can never disagree.
        let shard_doc = super::sql_shards::shard_doc(&state)?;

        let client = self.client().await?;
        let expected = match expected_revision {
            Some(rev) => rev,
            None => self.load_versioned().await?.0,
        };
        let rows = client
            .execute(
                tombstone::GUARDED_SAVE_SQL,
                &[
                    &self.project_id,
                    &i32::try_from(state.schema_version).unwrap_or(i32::MAX),
                    &value,
                    &expected,
                    &shard_doc,
                ],
            )
            .await
            .map_err(|e| PortError::Backend(format!("upsert: {e}")))?;
        if rows == 0 {
            // Either a concurrent writer moved the revision, or a delete
            // tombstoned this project in another process. A fresh read (new
            // statement snapshot) tells the two apart.
            if tombstone::exists(&client, &self.project_id).await? {
                return Err(tombstone::refusal(&self.project_id));
            }
            return Err(PortError::Conflict(
                "state changed since last read (concurrent writer)".to_owned(),
            ));
        }
        self.mirror_save(&state).await;
        Ok(())
    }
}
