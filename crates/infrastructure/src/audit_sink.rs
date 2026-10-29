//! Audit sink adapters: an in-memory ring buffer for local/single-node use and
//! a Postgres-backed sink that persists the trail across restarts. Both satisfy
//! the same [`AuditPort`] contract, so the server picks a backend at runtime.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AuditPort, AuditRecord, AuditVerification};
use coxagent_application::PortError;
use deadpool_postgres::Pool;
use std::collections::VecDeque;
use std::sync::Mutex;

use crate::audit_chain::{row_hash, verify_chain, ChainedEntry, GENESIS_PREV};

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

/// Prune roughly every this many records (plus once at startup), so a
/// long-running hub does not accumulate rows past the retention window without
/// paying a DELETE on every single write.
const PRUNE_EVERY: usize = 100;

/// Postgres-backed audit sink — one row per entry in a shared table.
pub struct SqlAuditSink {
    pool: Pool,
    /// Retention window in days; older rows are pruned. `None` = keep forever.
    retention_days: Option<u32>,
    writes: std::sync::atomic::AtomicUsize,
    /// Serialises the read-tip → compute-hash → insert critical section so
    /// concurrent writers extend one chain rather than forking it.
    chain_lock: tokio::sync::Mutex<()>,
}

impl SqlAuditSink {
    /// Connect to `dsn`, ensure the audit table exists, and prune once against
    /// `retention_days` (compliance retention; `None` keeps rows forever).
    ///
    /// # Errors
    /// [`PortError::Backend`] if the pool or migration fails.
    pub async fn connect(dsn: &str, retention_days: Option<u32>) -> Result<Self, PortError> {
        let pool = crate::pg::pool(dsn, "audit")
            .await
            .map_err(PortError::Backend)?;
        let sink = Self {
            pool,
            retention_days,
            writes: std::sync::atomic::AtomicUsize::new(0),
            chain_lock: tokio::sync::Mutex::new(()),
        };
        sink.migrate().await?;
        sink.prune().await;
        Ok(sink)
    }

    /// Delete rows older than the retention window (best-effort, no-op when
    /// retention is unset). `at` is RFC3339, which sorts lexicographically.
    async fn prune(&self) {
        let Some(days) = self.retention_days else {
            return;
        };
        let cutoff = time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days));
        let Ok(cutoff) = cutoff.format(&time::format_description::well_known::Rfc3339) else {
            return;
        };
        if let Ok(client) = self.pool.get().await {
            let _ = client
                .execute("DELETE FROM audit_log WHERE at < $1", &[&cutoff])
                .await;
        }
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
                );
                 CREATE INDEX IF NOT EXISTS audit_log_at_idx ON audit_log (at);
                 -- Tamper-evidence chain. Columns are added for pre-chain
                 -- deployments; backfill_chain() then seals existing rows into a
                 -- valid chain so verification covers the whole history.
                 ALTER TABLE audit_log ADD COLUMN IF NOT EXISTS prev TEXT NOT NULL DEFAULT '';
                 ALTER TABLE audit_log ADD COLUMN IF NOT EXISTS hash TEXT NOT NULL DEFAULT '';",
            )
            .await
            .map_err(|e| PortError::Backend(format!("audit migrate: {e}")))?;
        self.backfill_chain().await
    }

    /// One-time seal: give every not-yet-chained row (`hash = ''`) a prev/hash
    /// that links it to the row before it, in id order. Idempotent — once a row
    /// has a hash it is never recomputed, so an established chain is immutable
    /// and later tampering is detectable. Rows already chained are skipped, so
    /// this is a no-op on every startup after the first.
    async fn backfill_chain(&self) -> Result<(), PortError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| PortError::Backend(format!("audit connection: {e}")))?;
        // The current chain tip: the highest-id row that already has a hash.
        let mut prev: String = client
            .query_opt(
                "SELECT hash FROM audit_log WHERE hash <> '' ORDER BY id DESC LIMIT 1",
                &[],
            )
            .await
            .map_err(|e| PortError::Backend(format!("audit tip: {e}")))?
            .map_or_else(|| GENESIS_PREV.to_owned(), |r| r.get(0));
        let rows = client
            .query(
                "SELECT id, at, \"user\", action, status FROM audit_log
                 WHERE hash = '' ORDER BY id ASC",
                &[],
            )
            .await
            .map_err(|e| PortError::Backend(format!("audit backfill scan: {e}")))?;
        for r in &rows {
            let id: i64 = r.get(0);
            let at: String = r.get(1);
            let user: String = r.get(2);
            let action: String = r.get(3);
            let status = u16::try_from(r.get::<_, i32>(4)).unwrap_or(0);
            let hash = row_hash(&prev, &at, &user, &action, status);
            client
                .execute(
                    "UPDATE audit_log SET prev = $1, hash = $2 WHERE id = $3",
                    &[&prev, &hash, &id],
                )
                .await
                .map_err(|e| PortError::Backend(format!("audit backfill write: {e}")))?;
            prev = hash;
        }
        Ok(())
    }
}

#[async_trait]
impl AuditPort for SqlAuditSink {
    async fn record(&self, entry: AuditRecord) {
        // Best-effort: a logging failure must never break the request.
        let status = i32::from(entry.status);
        // Extend the chain under the lock so concurrent writers see a stable
        // tip and cannot fork it (read-tip → hash → insert is one critical
        // section). The guard is held only for this short round-trip.
        {
            let _guard = self.chain_lock.lock().await;
            let Ok(client) = self.pool.get().await else {
                return;
            };
            let prev: String = client
                .query_opt("SELECT hash FROM audit_log ORDER BY id DESC LIMIT 1", &[])
                .await
                .ok()
                .flatten()
                .map(|r| r.get::<_, String>(0))
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| GENESIS_PREV.to_owned());
            let hash = row_hash(&prev, &entry.at, &entry.user, &entry.action, entry.status);
            let _ = client
                .execute(
                    "INSERT INTO audit_log (at, \"user\", action, status, prev, hash)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                    &[&entry.at, &entry.user, &entry.action, &status, &prev, &hash],
                )
                .await;
        }
        // Amortised pruning so long runs stay within the retention window.
        if self.retention_days.is_some()
            && self
                .writes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                % PRUNE_EVERY
                == PRUNE_EVERY - 1
        {
            self.prune().await;
        }
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

    async fn verify(&self) -> Result<AuditVerification, PortError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| PortError::Backend(format!("audit connection: {e}")))?;
        let rows = client
            .query(
                "SELECT at, \"user\", action, status, prev, hash
                 FROM audit_log ORDER BY id ASC",
                &[],
            )
            .await
            .map_err(|e| PortError::Backend(format!("audit verify select: {e}")))?;
        let chain: Vec<ChainedEntry> = rows
            .iter()
            .map(|r| ChainedEntry {
                at: r.get(0),
                user: r.get(1),
                action: r.get(2),
                status: u16::try_from(r.get::<_, i32>(3)).unwrap_or(0),
                prev: r.get(4),
                hash: r.get(5),
            })
            .collect();
        Ok(match verify_chain(&chain) {
            Ok(n) => AuditVerification {
                supported: true,
                verified: n,
                intact: true,
                broken_at: None,
                detail: None,
            },
            Err(b) => AuditVerification {
                supported: true,
                verified: chain.len(),
                intact: false,
                broken_at: Some(b.index),
                detail: Some(b.reason),
            },
        })
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
