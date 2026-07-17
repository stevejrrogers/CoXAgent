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
    async fn save(&self, key: &str, json: &str) -> Result<(), PortError>;
}
