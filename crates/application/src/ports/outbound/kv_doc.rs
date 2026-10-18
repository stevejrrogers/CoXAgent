//! `KvDocPort` — a tiny key→JSON document store for hub-wide singletons that
//! don't belong to any one project (e.g. the system-wide chat aggregate). When
//! a backend (Postgres) is configured it's the system of record; otherwise the
//! caller falls back to a local file. Values are opaque JSON blobs.

use crate::error::PortError;
use async_trait::async_trait;

/// Persistence for a single JSON document addressed by an opaque string key.
#[async_trait]
pub trait KvDocPort: Send + Sync {
    /// Load the document for `key`, or `None` if absent.
    async fn load(&self, key: &str) -> Result<Option<String>, PortError>;

    /// Create or replace the document at `key`.
    ///
    /// Legacy last-writer-wins path, kept intact for callers that never
    /// captured a version (see [`Self::save_expecting`]).
    async fn save(&self, key: &str, json: &str) -> Result<(), PortError>;

    /// Persist the document at `key` expecting that no other writer advanced
    /// past `expected_revision` since this caller captured it from its own
    /// read.
    ///
    /// Optimistic concurrency control for hub-wide singletons shared by
    /// several hub replicas (CXA-C017, the `StateStorePort` CXA-F003 shape):
    /// two replicas that each read, mutate and plainly save can silently
    /// clobber each other's writes. Passing back the revision this caller saw
    /// when it loaded lets a backend that tracks revisions reject the stale
    /// write with [`PortError::Conflict`] before it overwrites newer data.
    /// Backends without revision tracking leave this at the default, which
    /// falls through to [`Self::save`] and preserves today's behaviour.
    async fn save_expecting(
        &self,
        key: &str,
        json: &str,
        _expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        self.save(key, json).await
    }

    /// The backend's current optimistic-concurrency revision for the document
    /// at `key`, if the backend tracks one (`None` otherwise).
    ///
    /// Hands back the value [`Self::save_expecting`] should be told this
    /// caller saw when it loaded. A revision-tracking backend reports the
    /// baseline `Some(0)` for an absent document — its first write lands at
    /// revision 1, matching the F003 shape. Callers treat `None` as "no guard
    /// available".
    async fn current_version(&self, _key: &str) -> Result<Option<i64>, PortError> {
        Ok(None)
    }
}
