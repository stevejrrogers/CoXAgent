//! `AuditPort` — the security-audit sink. An in-memory adapter serves local and
//! single-node use; a Postgres adapter persists the trail across restarts for a
//! multi-tenant hub. The server depends only on this boundary.

use crate::error::PortError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One audit record: an authenticated mutation or an auth attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// RFC3339 timestamp.
    pub at: String,
    /// The acting principal, or `anonymous` before authentication.
    pub user: String,
    /// A short description, e.g. `POST /api/projects/x/control/pause` or `login`.
    pub action: String,
    /// HTTP status of the outcome (200/403/401/…).
    pub status: u16,
}

/// Append-only security audit sink.
#[async_trait]
pub trait AuditPort: Send + Sync {
    /// Record one entry. Best-effort: implementations should not fail the caller.
    async fn record(&self, entry: AuditRecord);

    /// Return the most recent entries, newest first, capped at `limit`.
    ///
    /// # Errors
    /// [`PortError`] if the backing store cannot be read.
    async fn recent(&self, limit: usize) -> Result<Vec<AuditRecord>, PortError>;
}
