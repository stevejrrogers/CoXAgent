//! `PgKvDoc` — a Postgres-backed [`KvDocPort`]. Stores hub-wide JSON singletons
//! (e.g. the system chat aggregate) in one `app_kv` table so they live in the
//! shared database instead of a local file. Optimistic concurrency (a
//! monotonic `revision`) lets a replica guard its saves against concurrent
//! writers, mirroring the project state store.

use async_trait::async_trait;
use coxagent_application::ports::outbound::KvDocPort;
use coxagent_application::PortError;
use deadpool_postgres::{Config, Pool, Runtime};
use tokio_postgres::NoTls;

const INIT_SQL: &str = "
CREATE TABLE IF NOT EXISTS app_kv (
    key     TEXT PRIMARY KEY,
    doc     JSONB NOT NULL,
    updated TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Optimistic concurrency (CXA-C017): a monotonic revision every successful
-- write bumps, so hub replicas can guard saves the way SqlStateStore does
-- (CXA-F003). Idempotent and additive — pre-existing rows start at 0 and
-- older builds that ignore the column keep working unchanged.
ALTER TABLE app_kv ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 0;";

/// Postgres key→JSON store for hub-wide singletons.
pub struct PgKvDoc {
    pool: Pool,
}

impl PgKvDoc {
    /// Connect to `dsn` and ensure the `app_kv` table exists.
    ///
    /// # Errors
    /// Returns a message if the pool cannot be built or migration fails.
    pub async fn connect(dsn: &str) -> Result<Self, String> {
        let mut cfg = Config::new();
        cfg.url = Some(dsn.to_owned());
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| format!("kv pool: {e}"))?;
        let svc = Self { pool };
        svc.client()
            .await?
            .batch_execute(INIT_SQL)
            .await
            .map_err(|e| format!("kv migrate: {e}"))?;
        Ok(svc)
    }

    async fn client(&self) -> Result<deadpool_postgres::Client, String> {
        self.pool.get().await.map_err(|e| format!("kv conn: {e}"))
    }

    /// Insert-or-CAS-persist one document against a caller-chosen expected
    /// revision — the same atomic UPSERT shape
    /// `state::SqlStateStore::persist_at_revision` uses for project aggregates.
    ///
    /// Insert when absent (landing at revision 1); otherwise bump the revision
    /// only if it has not moved past what this writer expected. With
    /// `Some(rev)` a stale writer affects zero rows and gets
    /// [`PortError::Conflict`], persisting nothing. With `None` (the legacy
    /// [`KvDocPort::save`] path) re-read at write time — sound for
    /// intra-process writers that serialize on their handle's mutex. The
    /// guarded and legacy paths share one bump rule, so the counter stays
    /// monotonic no matter which path a writer took.
    async fn persist_at_revision(
        &self,
        key: &str,
        json: &str,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        let client = self.client().await.map_err(PortError::Backend)?;
        let expected = if let Some(rev) = expected_revision {
            rev
        } else {
            let row = client
                .query_opt("SELECT revision FROM app_kv WHERE key = $1", &[&key])
                .await
                .map_err(|e| PortError::Backend(format!("kv re-read: {e}")))?;
            row.map_or(0_i64, |r| r.get::<_, i64>(0))
        };
        let rows = client
            .execute(
                // `$2::text::jsonb`: bind as TEXT (a &str param), cast in SQL — a bare
                // `$2::jsonb` makes the driver infer a JSONB param and fail to
                // serialize a &str ("error serializing parameter").
                "INSERT INTO app_kv (key, doc, updated, revision)
                 VALUES ($1, $2::text::jsonb, now(), 1)
                 ON CONFLICT (key) DO UPDATE
                    SET doc = EXCLUDED.doc,
                        updated = now(),
                        revision = app_kv.revision + 1
                    WHERE app_kv.revision = $3",
                &[&key, &json, &expected],
            )
            .await
            .map_err(|e| PortError::Backend(format!("kv save: {e}")))?;
        if rows == 0 {
            return Err(PortError::Conflict(
                "kv doc changed since last read (concurrent writer)".to_owned(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl KvDocPort for PgKvDoc {
    async fn load(&self, key: &str) -> Result<Option<String>, PortError> {
        let client = self.client().await.map_err(PortError::Backend)?;
        let row = client
            .query_opt("SELECT doc::text FROM app_kv WHERE key = $1", &[&key])
            .await
            .map_err(|e| PortError::Backend(format!("kv load: {e}")))?;
        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    async fn save(&self, key: &str, json: &str) -> Result<(), PortError> {
        // Legacy path kept intact for callers that never captured a version.
        // It still bumps the revision: a blind write landing between two
        // guarded writes must invalidate the older guard, not slip under it.
        self.persist_at_revision(key, json, None).await
    }

    async fn save_expecting(
        &self,
        key: &str,
        json: &str,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        // Cross-replica optimistic concurrency: use exactly what THIS caller
        // read rather than re-reading at write time.
        self.persist_at_revision(key, json, expected_revision).await
    }

    async fn current_version(&self, key: &str) -> Result<Option<i64>, PortError> {
        let client = self.client().await.map_err(PortError::Backend)?;
        let row = client
            .query_opt("SELECT revision FROM app_kv WHERE key = $1", &[&key])
            .await
            .map_err(|e| PortError::Backend(format!("kv version: {e}")))?;
        // An absent key has not been written yet -> baseline revision 0,
        // matching persist_at_revision's first insert (`revision = 1`).
        Ok(Some(row.map_or(0_i64, |r| r.get::<_, i64>(0))))
    }
}
