//! `SqlStateStore` — a Postgres-backed [`StateStorePort`] for multi-tenant
//! deployments. Each project's aggregate is one JSONB row keyed by project id,
//! so many projects share one database while staying isolated by key.
//!
//! This is the port swap the architecture promised: use cases are unchanged;
//! only the adapter differs from [`super::JsonStateStore`]. Optimistic
//! concurrency (a monotonic `revision`) rejects lost updates from two writers.

use async_trait::async_trait;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::{ProjectState, SCHEMA_VERSION};
use coxagent_application::PortError;
use deadpool_postgres::{Config, Pool, Runtime};
use tokio_postgres::NoTls;

/// Schema for the shared project table. Idempotent; run on connect.
const INIT_SQL: &str = "
CREATE TABLE IF NOT EXISTS project_state (
    project_id     TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    revision       BIGINT  NOT NULL DEFAULT 0,
    data           JSONB   NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);";

/// A [`StateStorePort`] storing one project aggregate per row in Postgres.
pub struct SqlStateStore {
    pool: Pool,
    project_id: String,
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
        };
        store.migrate().await?;
        Ok(store)
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
        let value = serde_json::to_value(state)
            .map_err(|e| PortError::Backend(format!("encode: {e}")))?;

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
        Ok(())
    }
}
