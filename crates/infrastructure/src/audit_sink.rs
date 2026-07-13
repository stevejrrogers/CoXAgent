//! Audit sink adapters: an in-memory ring buffer for local/single-node use and
//! a Postgres-backed sink that persists the trail across restarts. Both satisfy
//! the same [`AuditPort`] contract, so the server picks a backend at runtime.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AuditPort, AuditRecord};
use coxagent_application::PortError;
use deadpool_postgres::{Config, Pool, Runtime};
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio_postgres::NoTls;

/// Bound for the in-memory sink.
const MEM_CAP: usize = 1000;

/// In-memory append-only audit log (newest last). Lost on restart.
pub struct MemoryAuditSink {
    entries: Mutex<VecDeque<AuditRecord>>,
}

impl Default for MemoryAuditSink {
    fn default() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
        }
    }
}

#[async_trait]
impl AuditPort for MemoryAuditSink {
    async fn record(&self, entry: AuditRecord) {
        if let Ok(mut buf) = self.entries.lock() {
            buf.push_back(entry);
            while buf.len() > MEM_CAP {
                buf.pop_front();
            }
        }
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditRecord>, PortError> {
        let buf = self
            .entries
            .lock()
            .map_err(|e| PortError::Backend(e.to_string()))?;
        Ok(buf.iter().rev().take(limit).cloned().collect())
    }
}

/// Postgres-backed audit sink — one row per entry in a shared table.
pub struct SqlAuditSink {
    pool: Pool,
}

impl SqlAuditSink {
    /// Connect to `dsn` and ensure the audit table exists.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the pool or migration fails.
    pub async fn connect(dsn: &str) -> Result<Self, PortError> {
        let mut cfg = Config::new();
        cfg.url = Some(dsn.to_owned());
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| PortError::Backend(format!("audit pool: {e}")))?;
        let sink = Self { pool };
        sink.migrate().await?;
        Ok(sink)
    }

    async fn migrate(&self) -> Result<(), PortError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| PortError::Backend(format!("audit connection: {e}")))?;
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS audit_log (
                    id      BIGSERIAL PRIMARY KEY,
                    at      TEXT NOT NULL,
                    \"user\" TEXT NOT NULL,
                    action  TEXT NOT NULL,
                    status  INTEGER NOT NULL
                );",
            )
            .await
            .map_err(|e| PortError::Backend(format!("audit migrate: {e}")))
    }
}

#[async_trait]
impl AuditPort for SqlAuditSink {
    async fn record(&self, entry: AuditRecord) {
        // Best-effort: a logging failure must never break the request.
        let Ok(client) = self.pool.get().await else {
            return;
        };
        let status = i32::from(entry.status);
        let _ = client
            .execute(
                "INSERT INTO audit_log (at, \"user\", action, status) VALUES ($1, $2, $3, $4)",
                &[&entry.at, &entry.user, &entry.action, &status],
            )
            .await;
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditRecord>, PortError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| PortError::Backend(format!("audit connection: {e}")))?;
        let capped = i64::try_from(limit).unwrap_or(1000);
        let rows = client
            .query(
                "SELECT at, \"user\", action, status FROM audit_log ORDER BY id DESC LIMIT $1",
                &[&capped],
            )
            .await
            .map_err(|e| PortError::Backend(format!("audit select: {e}")))?;
        Ok(rows
            .iter()
            .map(|r| AuditRecord {
                at: r.get(0),
                user: r.get(1),
                action: r.get(2),
                status: u16::try_from(r.get::<_, i32>(3)).unwrap_or(0),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_sink_bounds_and_orders_newest_first() {
        let sink = MemoryAuditSink::default();
        for i in 0..(MEM_CAP + 5) {
            sink.record(AuditRecord {
                at: format!("t{i}"),
                user: "u".to_owned(),
                action: format!("a{i}"),
                status: 200,
            })
            .await;
        }
        let recent = sink.recent(3).await.unwrap();
        assert_eq!(recent.len(), 3);
        // Newest first: the last inserted is a{MEM_CAP+4}.
        assert_eq!(recent[0].action, format!("a{}", MEM_CAP + 4));
    }
}
