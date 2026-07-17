//! `PgKvDoc` — a Postgres-backed [`KvDocPort`]. Stores hub-wide JSON singletons
//! (e.g. the system chat aggregate) in one `app_kv` table so they live in the
//! shared database instead of a local file.

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
);";

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
        let client = self.client().await.map_err(PortError::Backend)?;
        client
            // `$2::text::jsonb`: bind as TEXT (a &str param), cast in SQL — a bare
            // `$2::jsonb` makes the driver infer a JSONB param and fail to
            // serialize a &str ("error serializing parameter").
            .execute(
                "INSERT INTO app_kv (key, doc, updated) VALUES ($1, $2::text::jsonb, now())
                 ON CONFLICT (key) DO UPDATE SET doc = EXCLUDED.doc, updated = now()",
                &[&key, &json],
            )
            .await
            .map_err(|e| PortError::Backend(format!("kv save: {e}")))?;
        Ok(())
    }
}
