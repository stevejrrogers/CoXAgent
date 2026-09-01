//! `OutboxStorePort` — durable spool for outbound alerts (CXA-F235).
//!
//! The notifier enqueues here instead of POSTing fire-and-forget, and an
//! independent flusher claims due entries, POSTs them, and records the
//! outcome — so a down or slow webhook retries with backoff instead of
//! silently dropping an operator-facing alert. Implementations must be
//! crash-safe (an entry survives a restart until acknowledged) and bound the
//! spool by dropping the oldest entries under sustained outage.
//!
//! Best-effort like [`super::NotifierPort`]: methods never fail the caller;
//! adapters log persistence failures internally.

use crate::state::{unix_now_secs, OutboxEntry, OutboxStatus};
use async_trait::async_trait;
use std::sync::Mutex;

/// Durable delivery spool for outbound alerts.
///
/// Identities are store-assigned: an enqueued draft carries `id: 0` and the
/// implementation stamps a permanent, auto-incrementing id — that id is the
/// idempotency key receivers see on every retry.
#[async_trait]
pub trait OutboxStorePort: Send + Sync {
    /// Spool one event for delivery. Never blocks the caller on webhook state;
    /// a full spool drops the oldest entries instead of refusing the event.
    async fn enqueue(&self, entry: OutboxEntry);

    /// Atomically lease up to `batch` due entries (status pending, deadline
    /// passed), oldest first. Each claimed entry's deadline moves forward by
    /// the implementation's lease TTL — a flusher that crashes mid-POST loses
    /// the lease, and the entry becomes claimable again (at-least-once, hence
    /// the idempotency key).
    async fn claim_due(&self, batch: u32) -> Vec<OutboxEntry>;

    /// Record an acknowledgement (2xx): the entry becomes delivered history.
    async fn mark_delivered(&self, id: u64);

    /// Record a failed attempt: attempts increment, the deadline moves to
    /// `next_attempt_at` (unix seconds).
    async fn mark_retry(&self, id: u64, next_attempt_at: i64);

    /// Park an exhausted entry as dead, pending operator replay.
    async fn mark_dead(&self, id: u64);

    /// Newest entries first, up to `limit` — the acknowledgement history the
    /// operator view lists.
    async fn recent(&self, limit: u32) -> Vec<OutboxEntry>;

    /// Requeue a dead entry for immediate delivery (attempts reset). `false`
    /// when the id is unknown or the entry is not dead.
    async fn replay(&self, id: u64) -> bool;
}

/// In-memory [`OutboxStorePort`] — the pure test double, and the fallback for
/// embedders that serve the operator view without a durable spool (entries
/// then live only for this process's lifetime, exactly like the old
/// fire-and-forget notifier).
#[derive(Default)]
pub struct MemoryOutboxStore {
    inner: Mutex<MemorySpool>,
}

#[derive(Default)]
struct MemorySpool {
    next_id: u64,
    entries: Vec<OutboxEntry>,
}

impl MemoryOutboxStore {
    /// An empty spool.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OutboxStorePort for MemoryOutboxStore {
    async fn enqueue(&self, entry: OutboxEntry) {
        if let Ok(mut spool) = self.inner.lock() {
            spool.next_id += 1;
            let mut entry = entry;
            entry.id = spool.next_id;
            spool.entries.push(entry);
            // Bounded like every real spool: drop the oldest beyond the cap.
            while spool.entries.len() > 500 {
                spool.entries.remove(0);
            }
        }
    }

    async fn claim_due(&self, batch: u32) -> Vec<OutboxEntry> {
        let now = unix_now_secs();
        let Ok(mut spool) = self.inner.lock() else {
            return Vec::new();
        };
        let mut claimed = Vec::new();
        for e in spool
            .entries
            .iter_mut()
            .filter(|e| e.status == OutboxStatus::Pending && e.next_attempt_at <= now)
            .take(batch as usize)
        {
            e.next_attempt_at = now + 60; // the in-flight lease
            claimed.push(e.clone());
        }
        claimed
    }

    async fn mark_delivered(&self, id: u64) {
        if let Ok(mut spool) = self.inner.lock() {
            for e in spool.entries.iter_mut().filter(|e| e.id == id) {
                e.status = OutboxStatus::Delivered;
            }
        }
    }

    async fn mark_retry(&self, id: u64, next_attempt_at: i64) {
        if let Ok(mut spool) = self.inner.lock() {
            for e in spool.entries.iter_mut().filter(|e| e.id == id) {
                e.attempts = e.attempts.saturating_add(1);
                e.next_attempt_at = next_attempt_at;
            }
        }
    }

    async fn mark_dead(&self, id: u64) {
        if let Ok(mut spool) = self.inner.lock() {
            for e in spool.entries.iter_mut().filter(|e| e.id == id) {
                e.status = OutboxStatus::Dead;
            }
        }
    }

    async fn recent(&self, limit: u32) -> Vec<OutboxEntry> {
        let Ok(spool) = self.inner.lock() else {
            return Vec::new();
        };
        let mut entries = spool.entries.clone();
        entries.sort_by_key(|e| std::cmp::Reverse(e.id));
        entries.truncate(limit as usize);
        entries
    }

    async fn replay(&self, id: u64) -> bool {
        let Ok(mut spool) = self.inner.lock() else {
            return false;
        };
        let now = unix_now_secs();
        spool
            .entries
            .iter_mut()
            .filter(|e| e.id == id && e.status == OutboxStatus::Dead)
            .map(|e| {
                e.status = OutboxStatus::Pending;
                e.attempts = 0;
                e.next_attempt_at = now;
                true
            })
            .next()
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::OutboxStatus;
    use crate::state::OUTBOX_BASE_BACKOFF_SECS;

    fn event(kind: &str) -> OutboxEntry {
        OutboxEntry::pending(kind, "demo", "hello", 0)
    }

    #[tokio::test]
    async fn enqueue_assigns_a_permanent_idempotency_identity() {
        let spool = MemoryOutboxStore::new();
        spool.enqueue(event("deploy_failed")).await;
        spool.enqueue(event("budget_reached")).await;
        let recent = spool.recent(10).await;
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].kind, "budget_reached"); // newest first
        assert_ne!(recent[0].id, recent[1].id);
        assert_eq!(recent[1].status, OutboxStatus::Pending);
    }

    #[tokio::test]
    async fn claim_leases_so_a_second_take_finds_nothing() {
        let spool = MemoryOutboxStore::new();
        spool.enqueue(event("deploy_failed")).await;
        let first = spool.claim_due(10).await;
        assert_eq!(first.len(), 1);
        assert!(
            spool.claim_due(10).await.is_empty(),
            "leased, not re-claimed"
        );
    }

    #[tokio::test]
    async fn failed_attempt_schedules_the_backoff_deadline() {
        let spool = MemoryOutboxStore::new();
        spool.enqueue(event("deploy_failed")).await;
        let claimed = &spool.claim_due(10).await[0];
        let next = unix_now_secs() + OUTBOX_BASE_BACKOFF_SECS;
        spool.mark_retry(claimed.id, next).await;
        let after = &spool.recent(10).await[0];
        assert_eq!(after.attempts, 1);
        assert!(after.is_retrying());
        assert!(spool.claim_due(10).await.is_empty(), "backing off");
    }

    #[tokio::test]
    async fn replay_resets_only_dead_entries() {
        let spool = MemoryOutboxStore::new();
        spool.enqueue(event("deploy_failed")).await;
        let id = spool.claim_due(10).await[0].id;
        assert!(
            !spool.replay(id).await,
            "pending entries are not replayable"
        );
        spool.mark_dead(id).await;
        assert!(spool.replay(id).await);
        assert!(!spool.replay(u64::MAX).await);
        let replayed = &spool.recent(10).await[0];
        assert_eq!(replayed.status, OutboxStatus::Pending);
        assert_eq!(replayed.attempts, 0);
    }
}
