//! `StoragePort` — the boundary over blob storage for uploaded files (chat
//! attachments, discussion/ticket media). The default adapter writes to local
//! disk; an S3/MinIO adapter stores them in an object store instead. Keys are
//! slash-delimited paths like `chat/<file>` or `proj/<pid>/<file>`.

use crate::error::PortError;
use async_trait::async_trait;

/// Stores and retrieves uploaded file blobs by key.
#[async_trait]
pub trait StoragePort: Send + Sync {
    /// Store `data` under `key` with the given `mime` type.
    ///
    /// # Errors
    /// [`PortError`] when the write fails.
    async fn put(&self, key: &str, data: &[u8], mime: &str) -> Result<(), PortError>;

    /// Fetch the bytes stored under `key`.
    ///
    /// # Errors
    /// [`PortError`] when the object is missing or the read fails.
    async fn get(&self, key: &str) -> Result<Vec<u8>, PortError>;

    /// Whether the blob exists under `key` — the evidence forensics view's
    /// missing-artifact check (CXA-F241). Default probes via [`Self::get`]
    /// and discards the bytes; adapters with a cheap existence check
    /// override it.
    async fn exists(&self, key: &str) -> bool {
        self.get(key).await.is_ok()
    }
}
