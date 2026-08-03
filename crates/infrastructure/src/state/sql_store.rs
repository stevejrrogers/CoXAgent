//! `SqlStateStore` — a Postgres-backed [`StateStorePort`] for multi-tenant
//! deployments. Each project's aggregate is one JSONB row keyed by project id,
//! so many projects share one database while staying isolated by key.
//!
//! This is the port swap the architecture promised: use cases are unchanged;
//! only the adapter differs from [`super::JsonStateStore`]. Optimistic
//! concurrency (a monotonic `revision`) rejects lost updates from two writers.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{StateStorePort, WorkerEntry};
use coxagent_application::state::{ProjectState, SCHEMA_VERSION};
use coxagent_application::PortError;
use coxagent_domain::{Role, TicketId};
use deadpool_postgres::{Config, Pool, Runtime};
use tokio_postgres::NoTls;

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
ALTER TABLE project_coord ADD COLUMN IF NOT EXISTS models TEXT;";

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
}

impl SqlStateStore {
    /// Connect to `dsn` (a libpq/tokio-postgres URL) and ensure the schema
    /// exists, scoping this store to `project_id`.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the pool cannot be built or the schema
    /// migration fails.
    pub async fn connect(dsn: &str, project_id: impl Into<String>) -> Result<Self, PortError> {
        let mut cfg = Config::new();
        cfg.url = Some(dsn.to_owned());
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| PortError::Backend(format!("pool: {e}")))?;
        let store = Self {
            pool,
            project_id: project_id.into(),
            redis: None,
            local_mirror: None,
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
            .map_err(|e| PortError::Backend(format!("migrate: {e}")))
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
        state
            .validate()
            .map_err(|e| PortError::Corrupt(format!("refusing to save invalid state: {e}")))?;
        let value =
            serde_json::to_value(state).map_err(|e| PortError::Backend(format!("encode: {e}")))?;

        let client = self.client().await?;
        // Optimistic concurrency: insert when absent, otherwise bump the
        // revision only if it has not moved since we last read it. A concurrent
        // writer that advanced the revision makes this affect zero rows.
        let (expected, _) = self.load_versioned().await?;
        let rows = client
            .execute(
                "INSERT INTO project_state (project_id, schema_version, revision, data)
                 VALUES ($1, $2, 1, $3)
                 ON CONFLICT (project_id) DO UPDATE
                    SET data = EXCLUDED.data,
                        schema_version = EXCLUDED.schema_version,
                        revision = project_state.revision + 1,
                        updated_at = now()
                    WHERE project_state.revision = $4",
                &[
                    &self.project_id,
                    &i32::try_from(state.schema_version).unwrap_or(i32::MAX),
                    &value,
                    &expected,
                ],
            )
            .await
            .map_err(|e| PortError::Backend(format!("upsert: {e}")))?;
        if rows == 0 {
            return Err(PortError::Conflict(
                "state changed since last read (concurrent writer)".to_owned(),
            ));
        }
        self.mirror_save(state).await;
        Ok(())
    }

    async fn claim_ticket(
        &self,
        id: &TicketId,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
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
        tx.execute(
            "UPDATE project_state
                SET data = $1, revision = revision + 1, updated_at = now()
              WHERE project_id = $2",
            &[&newval, &self.project_id],
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
        engines: &[String],
        models: &[String],
        now: &str,
    ) -> Result<(), PortError> {
        if let Some(r) = &self.redis {
            return r
                .heartbeat_worker(worker, role, ticket, engines, models, now)
                .await;
        }
        let client = self.client().await?;
        let engines_csv = engines.join(",");
        let models_csv = models.join("\n");
        client
            .execute(
                "INSERT INTO project_coord
                    (project_id, kind, coord_key, worker, at, role, ticket, engines, models)
                 VALUES ($1, 'worker', $2, $2, now(), $3, $4, $5, $6)
                 ON CONFLICT (project_id, kind, coord_key) DO UPDATE
                    SET at = now(), role = EXCLUDED.role, ticket = EXCLUDED.ticket,
                        engines = EXCLUDED.engines, models = EXCLUDED.models",
                &[
                    &self.project_id,
                    &worker,
                    &role,
                    &ticket,
                    &engines_csv,
                    &models_csv,
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
                        coalesce(engines,''), coalesce(models,'')
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
}
