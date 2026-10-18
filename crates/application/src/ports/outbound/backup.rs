//! `BackupArchivePort` — the hub's own workspace state as one restorable
//! archive (CXA-F262).
//!
//! The hub's entire value lives in a mutable state directory: the project
//! registry, per-project configs and aggregates (tickets, evidence records,
//! wiki pages), the account file, the coordination/auth wiring, and the
//! locally-stored evidence blob bytes. Nothing but `JsonStateStore`'s
//! single-file `.backups/` ratchet protected any of it — a dead disk or a
//! botched upgrade erased months of autonomous-team history, and there was no
//! way to move a workspace to a new machine.
//!
//! This port is the archive seam: the use cases decide WHAT is captured and
//! WHETHER a restore is safe (all pure decisions, testable with in-memory
//! doubles); the adapter decides HOW the archive sits on disk (versioned
//! JSON+hex container, owner-only 0600, published by atomic rename so a crash
//! mid-write can never leave a partial archive as the newest restorable one).
//!
//! Versioning: [`ARCHIVE_SCHEMA_VERSION`] is bumped ONLY when the container
//! format changes meaning. Restore refuses a *newer* archive with a clear
//! error (the same ratchet `parse_checked` applies to state.json) so a
//! downgraded binary never half-understands a future format.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::path::Path;

/// The container format this crate writes and understands.
pub const ARCHIVE_SCHEMA_VERSION: u32 = 1;

/// Which side of the machine an [`ArchiveFile`] restores onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveRoot {
    /// Restored relative to the target hub directory.
    Workspace,
    /// Restored relative to the CURRENT machine's deploy-secrets root
    /// (`deploy_secrets_root()`), never to the source machine's path — the
    /// CXA-B032 keying (compose project name, not path) keeps the restored
    /// secrets valid on the new machine.
    Secrets,
}

impl ArchiveRoot {
    /// Stable tag used both in the serialized container and in the manifest
    /// integrity material.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Secrets => "secrets",
        }
    }
}

/// Whether a class of secret material shipped in the archive. The backup
/// output states the applied choice, so the operator never has to unzip and
/// check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretChoice {
    /// The material is NOT in the archive (the default and today's only mode
    /// for live session tokens).
    Excluded,
    /// The material IS in the archive, plaintext, in an owner-only (0600)
    /// file — the operator opted in with `--include-secrets`.
    Included,
}

/// Self-describing metadata at the top of every archive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArchiveManifest {
    /// Container format version; restore refuses a newer one.
    pub archive_schema: u32,
    /// RFC3339 instant the archive was taken.
    pub created_at: String,
    pub file_count: usize,
    pub total_bytes: u64,
    /// Integrity seal over the ordered file set — see [`archive_sha256`].
    pub sha256: String,
    /// Live bearer tokens (the sessions file, the operator token) — always
    /// [`SecretChoice::Excluded`]: restoring them would resurrect stale
    /// credentials and they are pure plaintext today.
    pub session_tokens: SecretChoice,
    /// The machine's deploy-secrets root.
    pub deploy_secrets: SecretChoice,
}

/// One captured file: where it belongs ([`ArchiveRoot`]), its archive-relative
/// path (forward-slash, verified safe — see [`is_safe_archive_path`]), hex
/// bytes, and its own sha256.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArchiveFile {
    pub root: ArchiveRoot,
    pub path: String,
    pub bytes_hex: String,
    pub sha256: String,
}

/// The whole restorable unit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceArchive {
    pub manifest: ArchiveManifest,
    pub files: Vec<ArchiveFile>,
}

/// Lowercase hex of `bytes` (the container is JSON, so bytes travel as hex).
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Inverse of [`hex_encode`]; `None` on odd length or a non-hex character —
/// a tampered archive must never half-decode.
#[must_use]
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let pair = |hi: u8, lo: u8| -> Option<u8> {
        let d = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        };
        Some((d(hi)? << 4) | d(lo)?)
    };
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair_at in (0..bytes.len()).step_by(2) {
        out.push(pair(bytes[pair_at], bytes[pair_at + 1])?);
    }
    Some(out)
}

/// sha256 of raw bytes, lowercase hex — the one hash definition every layer
/// shares (manifest seal, per-file seal, output line).
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

/// The archive's integrity seal: a digest over the ORDERED (root, path,
/// per-file-sha) list. Tampering with any file's bytes breaks its per-file
/// sha256; tampering with a path, a root, or a seal itself breaks this one —
/// restore verifies both before writing anything.
#[must_use]
pub fn archive_sha256(files: &[ArchiveFile]) -> String {
    let mut hasher = Sha256::new();
    for f in files {
        hasher.update(format!("{}/{}\n{}\n", f.root.tag(), f.path, f.sha256));
    }
    hex_encode(&hasher.finalize())
}

/// Is this archive-relative path safe to write under a restore root?
///
/// The rule (enforced on SAVE and re-checked on RESTORE, before any write):
/// non-empty, relative, and every forward-slash segment is real — no empty
/// segments, no `.`, no `..`. Backslashes are rejected outright: this code
/// always writes `/`-separated relative paths, and a `\` is a separator on
/// Windows, where `a\..\..\x` would traverse. A hostile archive can therefore
/// never escape the restore root the way `../../etc/passwd` or an absolute
/// path would.
#[must_use]
pub fn is_safe_archive_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && std::path::Path::new(path).is_relative()
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Everything restore must verify BEFORE writing anything: the schema ratchet
/// (a newer archive is refused with a clear error), the two integrity seals,
/// and path safety. One call so a caller cannot half-validate.
///
/// # Errors
/// [`PortError::Corrupt`] when the archive is from a newer schema, fails an
/// integrity seal, or carries an unsafe path.
pub fn validate_archive(archive: &WorkspaceArchive) -> Result<(), crate::PortError> {
    let m = &archive.manifest;
    if m.archive_schema > ARCHIVE_SCHEMA_VERSION {
        return Err(crate::PortError::Corrupt(format!(
            "archive was written by a newer schema version (archive_schema {} > \
             supported {ARCHIVE_SCHEMA_VERSION}) — upgrade coxagent before restoring",
            m.archive_schema
        )));
    }
    if m.sha256 != archive_sha256(&archive.files) {
        return Err(crate::PortError::Corrupt(
            "archive integrity check failed: the manifest seal does not match the file set \
             — the archive is tampered or truncated"
                .to_owned(),
        ));
    }
    for f in &archive.files {
        if !is_safe_archive_path(&f.path) {
            return Err(crate::PortError::Corrupt(format!(
                "archive carries an unsafe path ({:?}) — refusing before any write",
                f.path
            )));
        }
        let bytes = hex_decode(&f.bytes_hex).ok_or_else(|| {
            crate::PortError::Corrupt(format!(
                "archive file {:?} has malformed hex payload — the archive is corrupt",
                f.path
            ))
        })?;
        if sha256_hex(&bytes) != f.sha256 {
            return Err(crate::PortError::Corrupt(format!(
                "archive file {:?} fails its integrity hash — the archive is tampered or \
                 truncated",
                f.path
            )));
        }
    }
    Ok(())
}

/// Read/write the archive container, and probe a filesystem advisory lock.
#[async_trait]
pub trait BackupArchivePort: Send + Sync {
    /// Persist `archive` at `path` atomically (temp file + rename) and
    /// owner-only (0600 — the archive always carries the account file's
    /// credential hashes, plus deploy secrets when opted in).
    ///
    /// # Errors
    /// [`PortError::Backend`] when the file cannot be written.
    async fn save(&self, archive: &WorkspaceArchive, path: &Path) -> Result<(), crate::PortError>;

    /// Parse the container at `path`. Integrity and schema validation are the
    /// CALLER's pure decision ([`validate_archive`]) — the adapter only
    /// reads and parses.
    ///
    /// # Errors
    /// [`PortError::NotFound`] / [`PortError::Corrupt`] when the file is
    /// missing or not a valid archive container.
    async fn load(&self, path: &Path) -> Result<WorkspaceArchive, crate::PortError>;

    /// Does another process hold the advisory lock at `lock_path` right now?
    /// Used before a restore to mechanically refuse against a hub that is
    /// mid-write (the store's `.state.lock`), instead of trusting the
    /// operator to have stopped it.
    async fn probe_locked(&self, lock_path: &Path) -> bool;
}
