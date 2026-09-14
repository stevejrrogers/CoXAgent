//! `JsonHubArchive` — the [`BackupArchivePort`] adapter over the real
//! filesystem (CXA-F262).
//!
//! Container: one versioned, self-describing JSON document (`magic` tag +
//! flattened [`WorkspaceArchive`]), file payloads hex-encoded so the archive
//! stays a single text file that survives any text-safe transfer channel.
//!
//! Durability rules mirror `json_store.rs`'s own write path: the archive is
//! written to a temp file in the destination directory, fsynced, then renamed
//! over the final name — rename within a directory is atomic, so a crash
//! mid-write can never leave a partial archive as the newest restorable one.
//! The file is clamped to owner-only 0600 before it ever carries data: the
//! archive always contains the account file's credential hashes, plus deploy
//! secrets when the operator opted in.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{BackupArchivePort, WorkspaceArchive};
use coxagent_application::PortError;
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

/// Self-identifying tag so a wrong file never parses into an archive.
const MAGIC: &str = "coxagent-hub-archive";

#[derive(Serialize, Deserialize)]
struct Container {
    magic: String,
    archive_schema: u32,
    #[serde(flatten)]
    archive: WorkspaceArchive,
}

/// Filesystem-backed archive store.
#[derive(Default)]
pub struct JsonHubArchive;

impl JsonHubArchive {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait]
impl BackupArchivePort for JsonHubArchive {
    async fn save(&self, archive: &WorkspaceArchive, path: &Path) -> Result<(), PortError> {
        let doc = Container {
            magic: MAGIC.to_owned(),
            archive_schema: archive.manifest.archive_schema,
            archive: archive.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&doc)
            .map_err(|e| PortError::Backend(format!("archive serialization failed: {e}")))?;
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || write_atomic_0600(&path, &bytes))
            .await
            .map_err(|e| PortError::Backend(format!("archive writer joined failed: {e}")))?
    }

    async fn load(&self, path: &Path) -> Result<WorkspaceArchive, PortError> {
        let path = path.to_path_buf();
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| PortError::NotFound(format!("cannot read {}: {e}", path.display())))?;
        let doc: Container = serde_json::from_slice(&bytes).map_err(|e| {
            PortError::Corrupt(format!(
                "{} is not a coxagent hub archive: {e}",
                path.display()
            ))
        })?;
        if doc.magic != MAGIC {
            return Err(PortError::Corrupt(format!(
                "{} is not a coxagent hub archive (unrecognized container)",
                path.display()
            )));
        }
        if doc.archive_schema != doc.archive.manifest.archive_schema {
            // A container header that disagrees with the manifest is itself
            // corruption — the manifest is the authority.
            return Err(PortError::Corrupt(format!(
                "{}: container schema {} disagrees with manifest schema {}",
                path.display(),
                doc.archive_schema,
                doc.archive.manifest.archive_schema
            )));
        }
        Ok(doc.archive)
    }

    /// Try to take the advisory lock at `lock_path` and release it again:
    /// success means nobody holds it (`false`), failure means another
    /// process does (`true`). The file is created when missing — exactly
    /// what `acquire_lock` in the state store does — so probing a store
    /// root that has never been written is not an error.
    async fn probe_locked(&self, lock_path: &Path) -> bool {
        let path = lock_path.to_path_buf();
        tokio::task::spawn_blocking(move || probe_locked_blocking(&path))
            .await
            .unwrap_or(false)
    }
}

/// Blocking half of [`BackupArchivePort::probe_locked`].
fn probe_locked_blocking(lock_path: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
    else {
        return false; // cannot even open: nothing holds a lock we could observe
    };
    match file.try_lock_exclusive() {
        Ok(acquired) => {
            // Releasing is implicit: the lock dies with the file handle.
            drop(file);
            !acquired
        }
        Err(_) => true,
    }
}

/// Write `bytes` owner-only and atomically: temp file in the destination
/// directory (same filesystem), fsync, chmod 0600, rename over the target.
fn write_atomic_0600(path: &Path, bytes: &[u8]) -> Result<(), PortError> {
    let dir = path
        .parent()
        .ok_or_else(|| PortError::Backend(format!("{} has no parent dir", path.display())))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| PortError::Backend(format!("cannot create {}: {e}", dir.display())))?;
    let tmp = dir.join(format!(
        ".{}.tmp.{}",
        path.file_name().map_or_else(
            || "archive".to_owned(),
            |n| n.to_string_lossy().into_owned()
        ),
        std::process::id()
    ));
    let write = || -> Result<(), PortError> {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| PortError::Backend(format!("cannot create {}: {e}", tmp.display())))?;
        f.write_all(bytes)
            .map_err(|e| PortError::Backend(format!("cannot write {}: {e}", tmp.display())))?;
        f.sync_all()
            .map_err(|e| PortError::Backend(format!("cannot fsync {}: {e}", tmp.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| PortError::Backend(format!("cannot chmod 0600: {e}")))?;
        }
        std::fs::rename(&tmp, path)
            .map_err(|e| PortError::Backend(format!("cannot publish {}: {e}", path.display())))
    };
    let result = write();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp); // never leave tmp litter behind
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_application::ports::outbound::{
        archive_sha256, hex_encode, ArchiveFile, ArchiveManifest, SecretChoice, WorkspaceArchive,
        ARCHIVE_SCHEMA_VERSION,
    };
    use std::collections::BTreeSet;

    fn file(path: &str, bytes: &[u8]) -> ArchiveFile {
        ArchiveFile {
            root: coxagent_application::ports::outbound::ArchiveRoot::Workspace,
            path: path.to_owned(),
            bytes_hex: hex_encode(bytes),
            sha256: coxagent_application::ports::outbound::sha256_hex(bytes),
        }
    }

    fn archive(files: Vec<ArchiveFile>) -> WorkspaceArchive {
        WorkspaceArchive {
            manifest: ArchiveManifest {
                archive_schema: ARCHIVE_SCHEMA_VERSION,
                created_at: "2026-09-01T00:00:00Z".to_owned(),
                file_count: files.len(),
                total_bytes: files
                    .iter()
                    .map(|f| u64::try_from(f.bytes_hex.len() / 2).unwrap_or(u64::MAX))
                    .sum(),
                sha256: archive_sha256(&files),
                session_tokens: SecretChoice::Excluded,
                deploy_secrets: SecretChoice::Excluded,
            },
            files,
        }
    }

    /// Round-trip: save -> load is byte-equal, and the file on disk is a
    /// parseable container sealed for owner only (0600).
    #[tokio::test]
    async fn save_then_load_round_trips_byte_equal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("backups/coxagent-backup-x.hubarchive.json");
        let original = archive(vec![
            file("registry.json", br#"[{"id":"cxa","path":"/w/cxa"}]"#),
            file("blobs/evidence.png", b"\x89PNG-bytes\xff\x00"),
        ]);

        JsonHubArchive::new()
            .save(&original, &path)
            .await
            .expect("save");
        let loaded = JsonHubArchive::new().load(&path).await.expect("load");

        assert_eq!(loaded, original, "round-trip must be byte-equal");
        assert!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).expect("read"))
                .is_ok(),
            "the container is valid JSON"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the archive must be owner-only, got {mode:o}");
        }
    }

    /// A non-archive file loads as a clear corruption error, never as an
    /// archive with garbage inside.
    #[tokio::test]
    async fn a_non_archive_file_is_refused_with_a_clear_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-an-archive.json");
        std::fs::write(&path, b"{ \"hello\": \"world\" }").expect("write");

        let err = JsonHubArchive::new()
            .load(&path)
            .await
            .expect_err("not an archive");
        assert!(
            err.to_string().contains("not a coxagent hub archive"),
            "{err}"
        );
    }

    /// A missing archive is NotFound, with the path named.
    #[tokio::test]
    async fn a_missing_archive_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gone.hubarchive.json");
        let err = JsonHubArchive::new()
            .load(&path)
            .await
            .expect_err("missing archive");
        assert!(err.to_string().contains("gone.hubarchive.json"), "{err}");
    }

    /// The lock probe answers `false` on a free lock and `true` while another
    /// handle holds the fs4 exclusive lock — the mechanical running-hub check.
    #[tokio::test]
    async fn the_lock_probe_answers_true_only_while_the_lock_is_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("cxa/state/.state.lock");
        std::fs::create_dir_all(lock_path.parent().expect("parent")).expect("state dir");
        let adapter = JsonHubArchive::new();

        assert!(
            !adapter.probe_locked(&lock_path).await,
            "a free (or absent) lock is not locked"
        );

        let held = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .expect("open lock file");
        fs4::fs_std::FileExt::lock_exclusive(&held).expect("acquire");

        assert!(
            adapter.probe_locked(&lock_path).await,
            "a held lock must read as locked"
        );

        fs4::fs_std::FileExt::unlock(&held).expect("release");
        drop(held);
        assert!(
            !adapter.probe_locked(&lock_path).await,
            "after release the lock is free again"
        );
    }

    /// The saved container round-trips through serde with unique file paths
    /// intact — guards against accidental path mangling in the container.
    #[tokio::test]
    async fn container_paths_survive_the_round_trip_exactly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.hubarchive.json");
        let original = archive(vec![
            file("registry.json", b"[]"),
            file("cxa/state/state.json", b"{}"),
            file("cxa/state/project_context.md", b"# context"),
        ]);
        JsonHubArchive::new()
            .save(&original, &path)
            .await
            .expect("save");
        let loaded = JsonHubArchive::new().load(&path).await.expect("load");

        let before: BTreeSet<String> = original.files.iter().map(|f| f.path.clone()).collect();
        let after: BTreeSet<String> = loaded.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(before, after);
        assert_eq!(loaded.files.len(), 3);
    }
}
