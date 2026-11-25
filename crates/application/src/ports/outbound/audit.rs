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

/// Outcome of a tamper-evidence check over a persisted audit trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditVerification {
    /// Whether the backing store keeps a verifiable integrity chain at all.
    pub supported: bool,
    /// Number of rows checked (0 when unsupported or the log is empty).
    pub verified: usize,
    /// `true` when every checked row links correctly to the one before it.
    pub intact: bool,
    /// 0-based position of the first broken link, when `intact` is false.
    pub broken_at: Option<usize>,
    /// Human-readable detail (the break reason, or why it is unsupported).
    pub detail: Option<String>,
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

    /// Verify the integrity chain of the persisted trail, detecting any row
    /// that was edited, deleted, reordered, or truncated out of band. The
    /// default is "unsupported" — a backend with no durable chain (e.g. the
    /// in-memory sink) reports that rather than a false all-clear.
    ///
    /// # Errors
    /// [`PortError`] if the backing store cannot be read.
    async fn verify(&self) -> Result<AuditVerification, PortError> {
        Ok(AuditVerification {
            supported: false,
            verified: 0,
            intact: true,
            broken_at: None,
            detail: Some("this audit backend keeps no verifiable integrity chain".to_owned()),
        })
    }
}
