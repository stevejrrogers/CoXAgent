//! `FileOutboxStore` — the durable [`OutboxStorePort`] spool (CXA-F235).
//!
//! One JSON file per project (`<state dir>/outbox.json`), mutated under an
//! advisory file lock with atomic rename — the same crash-safety pattern as
//! `JsonStateStore` — so unacknowledged alerts survive a runner restart and
//! resume their retry schedule. Claims lease entries for a TTL (like the stage
//! claims in `json_store`), so a flusher that dies mid-POST loses only the
//! lease, never the alert: the entry becomes claimable again and the
//! idempotency key lets the receiver dedupe the double-send.
//!
//! The spool is bounded: enqueue drops the oldest entries beyond the cap, so a
//! sustained webhook outage can never grow the file unboundedly — and enqueue
//! itself is a quick local write that never waits on the webhook.

use crate::state::json_store::{acquire_lock, atomic_write};
use async_trait::async_trait;
use coxagent_application::ports::outbound::OutboxStorePort;
use coxagent_application::state::{
    clamp_message, unix_now_secs, OutboxEntry, OutboxStatus, OUTBOX_MAX_ATTEMPTS,
};
use coxagent_application::PortError;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Newest entries kept in the spool before the oldest are dropped. Large
/// enough for days of history under normal rates, small enough that reads and
/// writes stay trivial.
const DEFAULT_CAP: usize = 500;

/// How long a claim leases an entry before it is claimable again — comfortably
/// longer than the webhook POST timeout it covers.
const DEFAULT_LEASE_TTL_SECS: i64 = 60;

const SPOOL_FILE: &str = "outbox.json";

/// The persisted spool document.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Spool {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    entries: Vec<OutboxEntry>,
}

/// A durable, file-backed outbox spool for one project.
pub struct FileOutboxStore {
    path: PathBuf,
    cap: usize,
    lease_ttl_secs: i64,
}

impl FileOutboxStore {
    /// Create a spool persisting to `outbox.json` inside `dir`, creating the
    /// directory if needed.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the directory cannot be created.
    pub fn new(dir: impl AsRef<Path>) -> Result<Self, PortError> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|e| PortError::Backend(e.to_string()))?;
        Ok(Self {
            path: dir.join(SPOOL_FILE),
            cap: DEFAULT_CAP,
            lease_ttl_secs: DEFAULT_LEASE_TTL_SECS,
        })
    }

    /// Override the newest-entry cap. Production uses the default; tests
    /// exercise the drop-oldest rule on a tiny spool.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn with_cap(mut self, cap: usize) -> Self {
        self.cap = cap;
        self
    }

    /// Override the claim lease TTL. Production uses the default; tests
    /// exercise crash-recovery without waiting out the production 60s.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn with_lease_ttl_secs(mut self, ttl: i64) -> Self {
        self.lease_ttl_secs = ttl;
        self
    }

    fn params(&self) -> (PathBuf, usize, i64) {
        (self.path.clone(), self.cap, self.lease_ttl_secs)
    }
}

/// Read the spool file, or the empty default when absent. A file that exists
/// but does not parse is logged and treated as empty — the next enqueue
/// rebuilds it, and delivery must never wedge on a corrupt spool.
fn load_spool(path: &Path) -> Spool {
    match std::fs::read(path) {
        Err(_) => Spool::default(), // absent: a fresh spool
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(spool) => spool,
            Err(e) => {
                tracing::warn!(
                    "outbox spool {} unread ({e}) — starting empty",
                    path.display()
                );
                Spool::default()
            }
        },
    }
}

/// Persist the spool atomically (temp file + fsync + rename). The caller holds
/// the lock, so concurrent writers serialize.
fn write_spool(path: &Path, spool: &Spool) {
    if let Ok(json) = serde_json::to_vec(spool) {
        if let Err(e) = atomic_write(path.parent().unwrap_or(Path::new(".")), path, &json) {
            tracing::error!("outbox spool write failed ({}): {e}", path.display());
        }
    }
}

/// Read-mutate-write the spool under the advisory file lock.
fn with_spool<T>(path: &Path, f: impl FnOnce(&mut Spool) -> T) -> Result<T, PortError> {
    let lock = acquire_lock(path)?;
    let mut spool = load_spool(path);
    let out = f(&mut spool);
    write_spool(path, &spool);
    drop(lock);
    Ok(out)
}

/// The port is best-effort by contract — a spool op that fails must never
/// fail the caller — but a swallowed failure still gets a trace on the log,
/// or a systematically broken disk would silently strand every alert.
fn logged<T: Default>(path: &Path, op: &str, result: Result<T, PortError>) -> T {
    match result {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("outbox {op} failed ({}): {e}", path.display());
            T::default()
        }
    }
}

#[async_trait]
impl OutboxStorePort for FileOutboxStore {
    async fn enqueue(&self, entry: OutboxEntry) {
        let (path, cap, _) = self.params();
        let result = tokio::task::spawn_blocking(move || {
            logged(
                &path,
                "enqueue",
                with_spool(&path, |spool| {
                    spool.next_id += 1;
                    let mut entry = entry;
                    entry.id = spool.next_id;
                    entry.message = clamp_message(&entry.message);
                    entry.status = OutboxStatus::Pending;
                    spool.entries.push(entry);
                    // Bounded under sustained outage: drop the OLDEST events
                    // (lowest id = oldest, whatever their status) beyond the cap.
                    if spool.entries.len() > cap {
                        let excess = spool.entries.len() - cap;
                        spool.entries.drain(..excess);
                    }
                }),
            );
        })
        .await;
        if let Err(e) = result {
            tracing::error!("outbox enqueue task failed: {e}");
        }
    }

    async fn claim_due(&self, batch: u32) -> Vec<OutboxEntry> {
        let (path, _, lease_ttl) = self.params();
        tokio::task::spawn_blocking(move || {
            logged(
                &path,
                "claim_due",
                with_spool(&path, |spool| {
                    let now = unix_now_secs();
                    let mut claimed = Vec::new();
                    for e in spool
                        .entries
                        .iter_mut()
                        .filter(|e| e.status == OutboxStatus::Pending && e.next_attempt_at <= now)
                    {
                        if claimed.len() >= batch as usize {
                            break;
                        }
                        e.next_attempt_at = now + lease_ttl; // the in-flight lease
                        claimed.push(e.clone());
                    }
                    claimed
                }),
            )
        })
        .await
        .unwrap_or_default()
    }

    async fn mark_delivered(&self, id: u64) {
        let (path, _, _) = self.params();
        let _ = tokio::task::spawn_blocking(move || {
            logged(
                &path,
                "mark_delivered",
                with_spool(&path, |spool| {
                    for e in spool.entries.iter_mut().filter(|e| e.id == id) {
                        e.status = OutboxStatus::Delivered;
                    }
                }),
            );
        })
        .await;
    }

    async fn mark_retry(&self, id: u64, next_attempt_at: i64) {
        let (path, _, _) = self.params();
        let _ = tokio::task::spawn_blocking(move || {
            logged(
                &path,
                "mark_retry",
                with_spool(&path, |spool| {
                    for e in spool.entries.iter_mut().filter(|e| e.id == id) {
                        // Attempts are capped in code: a retry can never push an
                        // entry past the max the delivery policy promises.
                        e.attempts = e.attempts.saturating_add(1).min(OUTBOX_MAX_ATTEMPTS);
                        e.next_attempt_at = next_attempt_at;
                    }
                }),
            );
        })
        .await;
    }

    async fn mark_dead(&self, id: u64) {
        let (path, _, _) = self.params();
        let _ = tokio::task::spawn_blocking(move || {
            logged(
                &path,
                "mark_dead",
                with_spool(&path, |spool| {
                    for e in spool.entries.iter_mut().filter(|e| e.id == id) {
                        e.status = OutboxStatus::Dead;
                    }
                }),
            );
        })
        .await;
    }

    async fn recent(&self, limit: u32) -> Vec<OutboxEntry> {
        let (path, _, _) = self.params();
        tokio::task::spawn_blocking(move || {
            logged(
                &path,
                "recent",
                with_spool(&path, |spool| {
                    let mut entries = spool.entries.clone();
                    entries.sort_by_key(|e| std::cmp::Reverse(e.id));
                    entries.truncate(limit as usize);
                    entries
                }),
            )
        })
        .await
        .unwrap_or_default()
    }

    async fn replay(&self, id: u64) -> bool {
        let (path, _, _) = self.params();
        tokio::task::spawn_blocking(move || {
            logged(
                &path,
                "replay",
                with_spool(&path, |spool| {
                    let now = unix_now_secs();
                    let replayed = spool
                        .entries
                        .iter_mut()
                        .find(|e| e.id == id && e.status == OutboxStatus::Dead);
                    match replayed {
                        Some(e) => {
                            e.status = OutboxStatus::Pending;
                            e.attempts = 0;
                            e.next_attempt_at = now;
                            true
                        }
                        None => false,
                    }
                }),
            )
        })
        .await
        .unwrap_or(false)
    }
}

/// Arc helper so the composition root can hand one store to the notifier, the
/// background flusher and the dashboard's history view.
#[must_use]
pub fn spool_in_dir(dir: impl AsRef<Path>) -> Arc<dyn OutboxStorePort> {
    match FileOutboxStore::new(dir) {
        Ok(store) => Arc::new(store),
        // An uncreatable spool dir falls back to the process-local double
        // rather than failing the runner: alerts keep flowing, just without
        // restart durability.
        Err(e) => {
            tracing::warn!("outbox dir unavailable ({e}) — falling back to in-memory spool");
            Arc::new(coxagent_application::ports::outbound::MemoryOutboxStore::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        tempfile::tempdir().expect("tempdir").keep()
    }

    fn event(kind: &str) -> OutboxEntry {
        OutboxEntry::pending(kind, "demo", "hello", 0)
    }

    #[tokio::test]
    async fn enqueue_claims_and_delivers_end_to_end() {
        let store = FileOutboxStore::new(dir()).expect("store");
        store.enqueue(event("deploy_failed")).await;
        let claimed = store.claim_due(10).await;
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].kind, "deploy_failed");
        assert_eq!(claimed[0].project, "demo");
        assert!(claimed[0].id > 0, "store assigns the idempotency id");
        store.mark_delivered(claimed[0].id).await;
        let history = store.recent(10).await;
        assert_eq!(history[0].status, OutboxStatus::Delivered);
        assert!(
            store.claim_due(10).await.is_empty(),
            "delivered stays delivered"
        );
    }

    #[tokio::test]
    async fn claim_includes_only_entries_past_their_deadline() {
        // CXA-F235 test plan: the claim window's lower bound is next_attempt_at.
        let store = FileOutboxStore::new(dir()).expect("store");
        store.enqueue(event("later")).await;
        let id = store.claim_due(10).await[0].id;
        store.mark_retry(id, unix_now_secs() + 3_600).await;
        assert!(store.claim_due(10).await.is_empty(), "backing off, not due");
        store.enqueue(event("due")).await;
        let due = store.claim_due(10).await;
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].kind, "due");
    }

    #[tokio::test]
    async fn a_claimed_entry_is_leased_until_the_ttl_expires() {
        let store = FileOutboxStore::new(dir())
            .expect("store")
            .with_lease_ttl_secs(1);
        store.enqueue(event("deploy_failed")).await;
        assert_eq!(store.claim_due(10).await.len(), 1);
        assert!(
            store.claim_due(10).await.is_empty(),
            "in flight: not re-claimed inside the lease"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        // A flusher that died mid-POST loses only the lease — the entry
        // resumes, at-least-once.
        assert_eq!(store.claim_due(10).await.len(), 1);
    }

    #[tokio::test]
    async fn entries_survive_a_restart_and_resume_their_schedule() {
        // CXA-F235 AC: restart mid-retry resumes unacknowledged events.
        let d = dir();
        let store = FileOutboxStore::new(&d).expect("store");
        store.enqueue(event("budget_reached")).await;
        let id = store.claim_due(10).await[0].id;
        store.mark_retry(id, unix_now_secs() - 1).await; // deadline already passed
        drop(store);
        // The runner restarts; a fresh instance opens the same spool.
        let restarted = FileOutboxStore::new(&d).expect("store");
        let resumed = restarted.claim_due(10).await;
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].kind, "budget_reached");
        assert_eq!(
            resumed[0].attempts, 1,
            "attempt count carried across restart"
        );
    }

    #[tokio::test]
    async fn the_spool_stays_bounded_by_dropping_the_oldest() {
        // CXA-F235 AC: backlog beyond the cap drops oldest events.
        let store = FileOutboxStore::new(dir()).expect("store").with_cap(3);
        for kind in ["a", "b", "c", "d", "e"] {
            store.enqueue(event(kind)).await;
        }
        let recent = store.recent(10).await;
        assert_eq!(recent.len(), 3);
        let kinds: Vec<&str> = recent.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["e", "d", "c"], "newest first; oldest dropped");
    }

    #[tokio::test]
    async fn two_workers_never_take_the_same_entry_twice() {
        // CXA-F235 test plan: atomic double-take leaves exactly one claimed.
        let d = dir();
        let a = FileOutboxStore::new(&d).expect("store");
        let b = FileOutboxStore::new(&d).expect("store");
        a.enqueue(event("deploy_failed")).await;
        let (ta, tb) = (
            tokio::spawn(async move { a.claim_due(10).await }),
            tokio::spawn(async move { b.claim_due(10).await }),
        );
        let (ra, rb) = tokio::join!(ta, tb);
        let total = ra.expect("task").len() + rb.expect("task").len();
        assert_eq!(total, 1, "exactly one worker won the claim");
    }

    #[tokio::test]
    async fn replay_resets_only_dead_entries_to_immediately_due() {
        let store = FileOutboxStore::new(dir()).expect("store");
        store.enqueue(event("deploy_failed")).await;
        let id = store.claim_due(10).await[0].id;
        store.mark_dead(id).await;
        assert!(!store.replay(u64::MAX).await, "unknown id");
        assert!(store.replay(id).await);
        let resumed = store.claim_due(10).await;
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].attempts, 0, "a replay is a fresh run");
    }

    #[tokio::test]
    async fn oversized_messages_are_clamped_at_enqueue() {
        let store = FileOutboxStore::new(dir()).expect("store");
        let long = "x".repeat(coxagent_application::state::OUTBOX_MESSAGE_CAP_BYTES + 50);
        store
            .enqueue(OutboxEntry::pending("deploy_failed", "demo", &long, 0))
            .await;
        let recent = store.recent(10).await;
        assert_eq!(
            recent[0].message.len(),
            coxagent_application::state::OUTBOX_MESSAGE_CAP_BYTES
        );
    }
}
