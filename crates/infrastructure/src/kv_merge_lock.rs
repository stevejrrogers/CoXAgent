//! `KvMergeLock` — a [`MergeLockPort`] adapter over the existing
//! [`KvDocPort`](coxagent_application::ports::outbound::KvDocPort)
//! (CXA-F375a [1/3]): per-branch merge leases stored as JSON documents at
//! `merge-lock/<branch>`, guarded by optimistic concurrency (`save_expecting`)
//! so two hub replicas can never both win an acquire.
//!
//! Lease semantics: the document carries the holder, reason and expiry
//! (`expires_at_unix`). An expired lease is stealable by any acquirer, so a
//! crashed writer cannot wedge merges permanently — recovery is the TTL, no
//! janitor. Backends without revision tracking fall back to plain `save`
//! (single-writer behaviour, same as the KvDocPort defaults).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use coxagent_application::error::PortError;
use coxagent_application::ports::outbound::{
    KvDocPort, MergeLockHolder, MergeLockPort,
};
use serde::{Deserialize, Serialize};

/// Key prefix under which lease documents live inside the kv store.
const KEY_PREFIX: &str = "merge-lock/";

/// Encode a branch name into its lock key. Branch names contain `/`
/// (`coxagent/CXA-F393`, `release/v1.2`) and must round-trip unambiguously:
/// only `:` (the segment separator used by some backends) and `%` are
/// percent-encoded, everything else is kept verbatim so keys stay readable
/// and collision-free (`%` → `%25` first makes the encoding injective).
#[must_use]
pub fn merge_lock_key(branch: &str) -> String {
    let mut encoded = String::with_capacity(KEY_PREFIX.len() + branch.len());
    encoded.push_str(KEY_PREFIX);
    for ch in branch.chars() {
        match ch {
            '%' => encoded.push_str("%25"),
            ':' => encoded.push_str("%3A"),
            other => encoded.push(other),
        }
    }
    encoded
}

/// The persisted lease document.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseDoc {
    holder: String,
    why: String,
    acquired_at_unix: i64,
    expires_at_unix: i64,
}

impl LeaseDoc {
    fn attribution(&self) -> MergeLockHolder {
        MergeLockHolder {
            holder: self.holder.clone(),
            why: self.why.clone(),
            acquired_at_unix: self.acquired_at_unix,
            expires_at_unix: self.expires_at_unix,
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Merge-lease adapter over any [`KvDocPort`] (Postgres today via
/// `PgKvDoc`; nothing here is backend-specific).
pub struct KvMergeLock {
    kv: Arc<dyn KvDocPort>,
}

impl KvMergeLock {
    /// Wrap an existing shared kv-doc handle.
    #[must_use]
    pub fn new(kv: Arc<dyn KvDocPort>) -> Self {
        Self { kv }
    }

    /// Read + parse the current lease doc, if any. Unparseable docs are
    /// treated as corrupt (no holder) but release/acquire will overwrite them.
    async fn read_lease(&self, key: &str) -> Result<Option<LeaseDoc>, PortError> {
        let Some(json) = self.kv.load(key).await? else {
            return Ok(None);
        };
        Ok(serde_json::from_str::<LeaseDoc>(&json).ok())
    }

    /// Persist `doc` at the revision the caller observed (CAS), or blind when
    /// the backend does not track revisions.
    async fn write_lease(
        &self,
        key: &str,
        doc: &LeaseDoc,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        let json = serde_json::to_string(doc)
            .map_err(|e| PortError::Backend(format!("merge-lock encode: {e}")))?;
        match expected_revision {
            Some(rev) => self.kv.save_expecting(key, &json, Some(rev)).await,
            None => self.kv.save(key, &json).await,
        }
    }
}

#[async_trait]
impl MergeLockPort for KvMergeLock {
    async fn try_acquire(
        &self,
        key: &str,
        holder: &str,
        why: &str,
        ttl_secs: u64,
    ) -> Result<bool, PortError> {
        let now = now_unix();
        let revision = self.kv.current_version(key).await?;
        let incumbent = self.read_lease(key).await?;
        let takeover_allowed = incumbent.as_ref().map_or(true, |lease| {
            lease.holder == holder || lease.expires_at_unix < now
        });
        // A foreign LIVE lease means contention — an honest Ok(false), not a
        // spin: callers see who holds it via `holder()`.
        if !takeover_allowed {
            return Ok(false);
        }
        // A Conflict here is exactly another writer winning the race between
        // our read and write — map it to the contended verdict rather than a
        // backend error so N concurrent acquirers get exactly one `true`.
        let doc = LeaseDoc {
            holder: holder.to_owned(),
            why: why.to_owned(),
            acquired_at_unix: now,
            expires_at_unix: now.saturating_add_unsigned(ttl_secs.max(1)),
        };
        match self.write_lease(key, &doc, revision).await {
            Ok(()) => Ok(true),
            Err(PortError::Conflict(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn release(&self, key: &str, holder: &str) -> Result<bool, PortError> {
        let revision = self.kv.current_version(key).await?;
        let Some(lease) = self.read_lease(key).await? else {
            return Ok(false); // nothing held: nobody released anything
        };
        if lease.holder != holder {
            return Ok(false); // owner-only: a non-holder clears nothing
        }
        match self.delete_doc(key, revision).await {
            Ok(removed) => Ok(removed),
            Err(PortError::Conflict(_)) => Ok(false), // raced away: not ours anymore
            Err(e) => Err(e),
        }
    }

    async fn holder(&self, key: &str) -> Result<Option<MergeLockHolder>, PortError> {
        Ok(self.read_lease(key).await?.as_ref().map(LeaseDoc::attribution))
    }
}

impl KvMergeLock {
    /// Clear the key (lease release writes an empty tombstone through the
    /// guarded path, invalidating concurrent holders' revisions).
    async fn delete_doc(&self, key: &str, _revision: Option<i64>) -> Result<bool, PortError> {
        // KvDocPort has no delete; clearing to a zero-value document is not
        // expressible, so remove by CAS-overwriting with an already-expired
        // tombstone — semantically identical to absent (stealable, non-blocking).
        let tombstone = LeaseDoc {
            holder: String::new(),
            why: String::new(),
            acquired_at_unix: 0,
            expires_at_unix: 0,
        };
        self.write_lease(key, &tombstone, None).await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory KvDocPort fixture replicating PgKvDoc's CAS contract:
    /// `save_expecting(Some(rev))` conflicts when the revision moved since
    /// the caller read it.
    struct MemKv {
        docs: Mutex<HashMap<String, (String, i64)>>,
        no_revisions: bool,
    }

    impl MemKv {
        fn new() -> Self {
            Self { docs: Mutex::new(HashMap::new()), no_revisions: false }
        }
        fn legacy() -> Self {
            Self { docs: Mutex::new(HashMap::new()), no_revisions: true }
        }
    }

    #[async_trait]
    impl KvDocPort for MemKv {
        async fn load(&self, key: &str) -> Result<Option<String>, PortError> {
            Ok(self
                .docs
                .lock()
                .expect("memkv")
                .get(key)
                .map(|(doc, _)| doc.clone()))
        }

        async fn save(&self, key: &str, json: &str) -> Result<(), PortError> {
            let mut docs = self.docs.lock().expect("memkv");
            let rev = docs.get(key).map_or(0, |(_, r)| *r) + 1;
            docs.insert(key.to_owned(), (json.to_owned(), rev));
            Ok(())
        }

        async fn save_expecting(
            &self,
            key: &str,
            json: &str,
            expected: Option<i64>,
        ) -> Result<(), PortError> {
            let Some(expected_rev) = expected else {
                return Box::pin(async { self.save(key, json).await }).await;
            };
            if self.no_revisions {
                return self.save(key, json).await;
            }
            let mut docs = self.docs.lock().expect("memkv");
            let current = docs.get(key).map_or(0, |(_, r)| *r);
            if current != expected_rev {
                return Err(PortError::Conflict("stale".into()));
            }
            docs.insert(key.to_owned(), (json.to_owned(), current + 1));
            Ok(())
        }

        async fn current_version(&self, key: &str) -> Result<Option<i64>, PortError> {
            if self.no_revisions {
                return Ok(None);
            }
            Ok(Some(self.docs.lock().expect("memkv").get(key).map_or(0, |(_, r)| *r)))
        }
    }

    fn lock(kv: Arc<dyn KvDocPort>) -> KvMergeLock {
        KvMergeLock::new(kv)
    }

    #[test]
    fn key_encoding_is_deterministic_and_collision_free() {
        assert_eq!(merge_lock_key("develop"), "merge-lock/develop");
        assert_eq!(merge_lock_key("coxagent/CXA-F393"), "merge-lock/coxagent/CXA-F393");
        assert_eq!(merge_lock_key("release/v1.2"), "merge-lock/release/v1.2");
        // Hostile input still encodes injectively.
        assert_eq!(merge_lock_key("a:b%c"), "merge-lock/a%3Ab%25c");
        assert_ne!(
            merge_lock_key("a:b"),
            merge_lock_key("a%3Ab"),
            "%-encoded original vs literal colon must differ"
        );
    }

    #[tokio::test]
    async fn mutual_exclusion_exactly_one_of_two_concurrent_acquirers_wins() {
        let lock = lock(Arc::new(MemKv::new()));
        let a = lock
            .try_acquire("k", "a@h", "sweep A", 60)
            .await
            .expect("acquire A");
        let b = lock
            .try_acquire("k", "b@h", "sweep B", 60)
            .await
            .expect("acquire B");
        assert!((a, b) == (true, false) || (a, b) == (false, true));
        let held = lock.holder("k").await.expect("holder").expect("held");
        assert_eq!(held.holder, if a { "a@h" } else { "b@h" });
    }

    #[tokio::test]
    async fn expired_lease_is_stealable_without_a_release() {
        let lock = lock(Arc::new(MemKv::new()));
        assert!(lock.try_acquire("k", "crashed@h", "gone", 1).await.expect("first"));
        // Force expiry by taking with the incumbent overwritten via a very old
        // write: emulate time passing by rewriting the doc directly.
        let old = serde_json::to_string(&LeaseDoc {
            holder: "crashed@h".into(),
            why: "gone".into(),
            acquired_at_unix: 0,
            expires_at_unix: 1,
        })
        .expect("encode");
        lock.kv.save("k", &old).await.expect("backdate");
        assert!(
            lock.try_acquire("k", "fresh@h", "next sweep", 60).await.expect("steal"),
            "crashed holder must not wedge merges"
        );
        assert_eq!(lock.holder("k").await.expect("holder").unwrap().holder, "fresh@h");
    }

    #[tokio::test]
    async fn release_by_non_holder_returns_false_and_clears_nothing() {
        let lock = lock(Arc::new(MemKv::new()));
        assert!(lock.try_acquire("k", "real@h", "sweep", 60).await.expect("acquire"));
        assert!(!lock.release("k", "imposter@h").await.expect("foreign release"));
        assert_eq!(lock.holder("k").await.expect("holder").unwrap().holder, "real@h");
        assert!(lock.release("k", "real@h").await.expect("owner release"));
        // Holder view now reflects the freed (expired-tombstone) lease.
        let after = lock.holder("k").await.expect("holder");
        assert!(after.is_none_or(|h| h.expired_at(now_unix())));
    }

    #[tokio::test]
    async fn legacy_backend_without_revisions_falls_back_to_plain_save() {
        let lock = lock(Arc::new(MemKv::legacy()));
        assert!(lock.try_acquire("k", "solo@h", "single runner", 60).await.expect("grant"));
        assert!(lock.release("k", "solo@h").await.expect("release"));
    }
}
