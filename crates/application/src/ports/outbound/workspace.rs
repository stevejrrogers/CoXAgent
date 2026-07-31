//! `WorkspaceFilesPort` — plain file reads and writes inside a project's
//! working directory (CLAUDE.md team notes, the agent memory index, generated
//! maps). The application decides WHAT a file should say; putting the reads
//! and writes behind this port keeps that decision testable with an in-memory
//! double and keeps `std::fs` out of the use-case layer, where the hexagonal
//! ratchet forbids it.

use async_trait::async_trait;
use std::path::Path;

/// Read/write access to files under a workspace root.
#[async_trait]
pub trait WorkspaceFilesPort: Send + Sync {
    /// The file's contents, or `None` when it does not exist or cannot be read.
    async fn read(&self, path: &Path) -> Option<String>;

    /// Write `content`, creating parent directories as needed. Returns whether
    /// the write landed.
    async fn write(&self, path: &Path, content: &str) -> bool;

    /// Write raw bytes (PNG evidence, binary artefacts), creating parent
    /// directories as needed. Returns whether the write landed.
    async fn write_bytes(&self, path: &Path, bytes: &[u8]) -> bool;

    /// Delete a file. Returns whether it was removed.
    async fn delete(&self, path: &Path) -> bool;

    /// The files directly inside `dir` (no recursion), with the metadata the
    /// memory-hygiene pass ranks by. Missing directory reads as empty.
    async fn list(&self, dir: &Path) -> Vec<FileMeta>;

    /// Metadata for one path, or `None` when it does not exist.
    async fn stat(&self, path: &Path) -> Option<FileMeta>;

    /// Every file under `dir`, recursively, as absolute paths. Missing
    /// directory reads as empty. Vendor/build directories are the CALLER's
    /// concern — the adapter reports what is there.
    async fn list_recursive(&self, dir: &Path) -> Vec<std::path::PathBuf>;

    /// The directories directly inside `dir` (no recursion), sorted by name.
    /// Missing directory reads as empty.
    async fn list_dirs(&self, dir: &Path) -> Vec<std::path::PathBuf>;
}

/// One file as [`WorkspaceFilesPort::list`] reports it.
#[derive(Debug, Clone)]
pub struct FileMeta {
    pub path: std::path::PathBuf,
    /// Seconds since the epoch of the last modification, 0 when unknown.
    pub modified_epoch: u64,
    pub size: u64,
}
