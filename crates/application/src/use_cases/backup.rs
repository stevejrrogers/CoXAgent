//! `BackupWorkspaceUseCase` — one restorable archive of the hub's own state
//! (CXA-F262).
//!
//! WHAT is captured is a pure spec over the hub layout the running code
//! actually reads and writes (`run_hub` boots from it, `load_coordination`
//! and `build_auth` read beside it):
//!
//! * hub-level files, each if present: `registry.json` (required — the
//!   project list every other capture hangs off), `coordination.json`,
//!   `auth.json` (salted credential hashes only), `system_chat.json`,
//!   `workspace.json` (company conventions in local mode);
//! * per registry project (when it lives under the hub dir — anything else
//!   is not portable and is skipped with a warning): `coxagent.json` and the
//!   whole `state/` tree (tickets, evidence records, wiki pages, the
//!   coordination sidecar), plus the local evidence blob bytes under
//!   `blobs/`;
//! * with `--include-secrets`, the machine's deploy-secrets root under
//!   [`ArchiveRoot::Secrets`].
//!
//! Secret policy (stated in the backup output, never left for the operator
//! to unzip and check): live bearer tokens — `sessions.json` and
//! `operator.token` — are EXCLUDED; deploy secrets are EXCLUDED unless opted
//! in; everything else ships as-is inside an owner-only (0600) archive.
//!
//! Torn files can never enter an archive: every `.json` capture is
//! parse-validated, re-read once on failure (a writer mid-rename heals on
//! the second read), and a still-invalid file FAILS the backup loudly — a
//! partial capture must not masquerade as a restorable archive. The per-file
//! integrity seals plus the manifest seal (both verified by
//! [`validate_archive`](coxagent_application::ports::outbound::validate_archive)
//! on restore) close the loop.

use crate::ports::outbound::WorkspaceFilesPort;
use crate::ports::outbound::{
    sha256_hex, ArchiveFile, ArchiveManifest, ArchiveRoot, BackupArchivePort, SecretChoice,
    WorkspaceArchive, ARCHIVE_SCHEMA_VERSION,
};
use crate::state::now_rfc3339;
use crate::AppError;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Delay before the one re-read of a JSON file that failed to parse — long
/// enough for a plain write that raced the capture to have finished.
const TORN_RECHECK: std::time::Duration = std::time::Duration::from_millis(150);

/// Where and how to take a backup.
#[derive(Debug, Clone)]
pub struct BackupRequest {
    /// The hub directory whose state is archived (the registry's parent).
    pub hub_dir: PathBuf,
    /// The archive file to write (under `<hub-dir>/backups/` by default).
    pub out: PathBuf,
    /// The machine's deploy-secrets root — `Some` only when the operator
    /// opted in with `--include-secrets`.
    pub secrets_root: Option<PathBuf>,
}

/// What one backup run produced.
#[derive(Debug, Clone)]
pub struct BackupOutcome {
    pub archive_path: PathBuf,
    pub manifest: ArchiveManifest,
    /// Captured paths, archive-relative, in the order sealed into the archive.
    pub captured: Vec<String>,
    /// Human-readable statement of the secret policy the run applied.
    pub secret_statement: String,
    /// Non-fatal observations (skipped non-portable projects, vanished
    /// transient files).
    pub warnings: Vec<String>,
}

/// The one restorable archive of the hub's own state.
pub struct BackupWorkspaceUseCase<W: WorkspaceFilesPort, A: BackupArchivePort> {
    files: Arc<W>,
    archive: Arc<A>,
}

impl<W: WorkspaceFilesPort, A: BackupArchivePort> BackupWorkspaceUseCase<W, A> {
    pub fn new(files: Arc<W>, archive: Arc<A>) -> Self {
        Self { files, archive }
    }

    /// Capture the hub's file-backed state into one archive.
    ///
    /// # Errors
    /// [`AppError`] when the registry is missing/unreadable/unparseable, a
    /// capture target cannot be read or fails JSON validation twice (torn or
    /// corrupt — never archived), or the archive cannot be persisted.
    pub async fn execute(&self, req: &BackupRequest) -> Result<BackupOutcome, AppError> {
        let mut workspace: Vec<(String, Vec<u8>)> = Vec::new();
        let mut warnings = Vec::new();

        // The registry is the spine: no registry, no restorable archive.
        let projects = self.read_registry(&req.hub_dir, &mut workspace).await?;

        // Hub-level state files, each if present.
        for name in [
            "coordination.json",
            "auth.json",
            "system_chat.json",
            "workspace.json",
        ] {
            if let Some(bytes) = self.capture_json(&req.hub_dir.join(name)).await? {
                workspace.push((name.to_owned(), bytes));
            }
        }

        // Per-project config + aggregate + local evidence blobs.
        for (id, path) in &projects {
            let Some(rel_dir) = project_rel_dir(&req.hub_dir, path) else {
                warnings.push(format!(
                    "project '{id}' lives outside the hub dir ({}) — its state is not \
                     portable and was not captured",
                    path.display()
                ));
                continue;
            };
            if let Some(bytes) = self.capture_json(&path.join("coxagent.json")).await? {
                workspace.push((format!("{rel_dir}/coxagent.json"), bytes));
            }
            self.capture_tree(&path.join("state"), &req.hub_dir, &mut workspace)
                .await?;
        }
        self.capture_tree(&req.hub_dir.join("blobs"), &req.hub_dir, &mut workspace)
            .await?;

        // Deploy secrets: only on explicit opt-in, under their own root.
        let mut secrets: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some(root) = &req.secrets_root {
            self.capture_tree(root, root, &mut secrets).await?;
        }

        let mut files: Vec<ArchiveFile> = workspace
            .iter()
            .map(|(path, bytes)| seal_file(ArchiveRoot::Workspace, path, bytes))
            .chain(
                secrets
                    .iter()
                    .map(|(path, bytes)| seal_file(ArchiveRoot::Secrets, path, bytes)),
            )
            .collect();
        files.sort_by(|a, b| (a.root.tag(), &a.path).cmp(&(b.root.tag(), &b.path)));

        let deploy_secrets = if secrets.is_empty() {
            SecretChoice::Excluded
        } else {
            SecretChoice::Included
        };
        let total_bytes = files
            .iter()
            .map(|f| u64::try_from(f.bytes_hex.len() / 2).unwrap_or(u64::MAX))
            .sum();
        let manifest = ArchiveManifest {
            archive_schema: ARCHIVE_SCHEMA_VERSION,
            created_at: now_rfc3339(),
            file_count: files.len(),
            total_bytes,
            sha256: crate::ports::outbound::archive_sha256(&files),
            session_tokens: SecretChoice::Excluded,
            deploy_secrets,
        };
        let archive = WorkspaceArchive { manifest, files };
        self.archive.save(&archive, &req.out).await?;

        let captured = archive
            .files
            .iter()
            .map(|f| format!("{}/{}", f.root.tag(), f.path))
            .collect();
        Ok(BackupOutcome {
            archive_path: req.out.clone(),
            manifest: archive.manifest,
            captured,
            secret_statement: secret_statement(deploy_secrets),
            warnings,
        })
    }

    /// Read a JSON capture target with the torn-read defence: validate, and
    /// on failure re-read once before giving up. `Ok(None)` = absent (fine).
    async fn capture_json(&self, abs: &Path) -> Result<Option<Vec<u8>>, AppError> {
        let Some(bytes) = self.files.read_bytes(abs).await else {
            return Ok(None);
        };
        if serde_json::from_slice::<serde_json::Value>(&bytes).is_ok() {
            return Ok(Some(bytes));
        }
        // A writer may have been mid-publish; a fresh read usually heals.
        tokio::time::sleep(TORN_RECHECK).await;
        match self.files.read_bytes(abs).await {
            Some(bytes) if serde_json::from_slice::<serde_json::Value>(&bytes).is_ok() => {
                Ok(Some(bytes))
            }
            _ => Err(AppError::Port(crate::PortError::Corrupt(format!(
                "{} is not valid JSON (torn or corrupt) — refusing to archive it; \
                 repair the file and retry",
                abs.display()
            )))),
        }
    }

    /// Read any non-JSON capture target (evidence blobs, plain files).
    async fn capture_file(&self, abs: &Path) -> Result<Vec<u8>, AppError> {
        self.files.read_bytes(abs).await.ok_or_else(|| {
            AppError::Port(crate::PortError::NotFound(format!(
                "cannot read {} — it vanished mid-capture or is unreadable",
                abs.display()
            )))
        })
    }

    /// Read and parse the hub registry — the spine of every capture. The
    /// registry text is pushed into `into` as the first captured file.
    async fn read_registry(
        &self,
        hub_dir: &Path,
        into: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<Vec<(String, PathBuf)>, AppError> {
        let registry_path = hub_dir.join("registry.json");
        let registry_text = self.files.read(&registry_path).await.ok_or_else(|| {
            AppError::Port(crate::PortError::NotFound(format!(
                "no hub registry at {} — this directory is not a hub workspace \
                 (pass --hub-dir with the registry's directory)",
                registry_path.display()
            )))
        })?;
        let projects = parse_registry(&registry_text).map_err(|e| {
            AppError::Port(crate::PortError::Corrupt(format!(
                "registry.json is not a valid hub registry ({e}) — fix or remove it \
                 before backing up"
            )))
        })?;
        into.push(("registry.json".to_owned(), registry_text.into_bytes()));
        Ok(projects)
    }

    /// Capture every non-excluded file under `tree`, stored relative to
    /// `hub_dir` (the archive's portability anchor).
    async fn capture_tree(
        &self,
        tree: &Path,
        hub_dir: &Path,
        into: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<(), AppError> {
        for abs in self.files.list_recursive(tree).await {
            let rel = rel_from(hub_dir, &abs);
            if is_excluded(&rel) {
                continue;
            }
            let bytes = self.capture_file(&abs).await?;
            into.push((rel, bytes));
        }
        Ok(())
    }
}

/// Seal one captured file: its own sha256 travels with its hex bytes so
/// restore can verify it before writing anything.
fn seal_file(root: ArchiveRoot, path: &str, bytes: &[u8]) -> ArchiveFile {
    ArchiveFile {
        root,
        path: path.to_owned(),
        bytes_hex: crate::ports::outbound::hex_encode(bytes),
        sha256: sha256_hex(bytes),
    }
}

/// The `(id, path)` pairs the hub registry carries.
fn parse_registry(text: &str) -> Result<Vec<(String, PathBuf)>, String> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let id = e
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or("entry without an id")?
            .to_owned();
        let path = e
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("entry '{id}' without a path"))?;
        out.push((id, PathBuf::from(path)));
    }
    Ok(out)
}

/// A project's archive-relative directory, but only when it lives UNDER the
/// hub dir — the archive must stay portable to a new machine, so an absolute
/// path outside the hub dir can never be expressed.
fn project_rel_dir(hub_dir: &Path, project: &Path) -> Option<String> {
    let rel = rel_from(hub_dir, project);
    if rel.is_empty() {
        None
    } else {
        Some(rel)
    }
}

/// One path as forward-slash text relative to `root`.
#[must_use]
pub fn rel_from(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default()
}

/// The capture exclusion list — the SAME list decides what a backup skips
/// and what a non-empty-target check ignores (backups/ and the pre-restore
/// snapshots must never nest into subsequent backups, live token files must
/// never be captured, and an archive itself never becomes part of a later
/// archive even if a stray `--out` once pointed inside the tree).
#[must_use]
pub fn is_excluded(rel: &str) -> bool {
    let segments: Vec<&str> = rel.split('/').collect();
    let name = segments.last().copied().unwrap_or_default();
    name == "sessions.json"
        || name == "operator.token"
        || name == ".state.lock"
        || name.starts_with(".state.tmp.")
        || name.ends_with(".hubarchive.json")
        || segments.contains(&".backups")
        || segments
            .iter()
            .any(|s| s.starts_with(".pre-restore-") || *s == "backups")
}

/// The human-readable statement of the secret policy a run applied — printed
/// in the backup output so the operator is told, not left to check.
#[must_use]
pub fn secret_statement(deploy_secrets: SecretChoice) -> String {
    let secrets = match deploy_secrets {
        SecretChoice::Excluded => "EXCLUDED (pass --include-secrets to capture them)",
        SecretChoice::Included => "INCLUDED (plaintext; the archive file is owner-only 0600)",
    };
    format!(
        "secret policy: session tokens EXCLUDED (sessions.json and operator.token are never \
         archived — live bearer material); auth.json included (salted credential hashes only); \
         coordination.json included verbatim (may carry DSN credentials — prefer ${{VAR}} \
         placeholders); deploy secrets {secrets}"
    )
}

/// RFC3339 -> filesystem-safe compact stamp (`2026-09-01T00:00:00Z` ->
/// `20260901T000000Z`), so archive and snapshot names sort chronologically
/// by name (the json_store prune convention).
#[must_use]
pub fn path_stamp(rfc3339: &str) -> String {
    rfc3339
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{hex_decode, validate_archive};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    // --- the two in-memory doubles (the MemStore pattern) -------------------

    #[derive(Default, Clone)]
    struct FakeFiles {
        files: BTreeMap<PathBuf, Vec<u8>>,
        /// Absolute paths handed to list_recursive, per root.
        trees: Arc<Mutex<BTreeMap<PathBuf, Vec<PathBuf>>>>,
    }

    impl FakeFiles {
        fn with_file(&mut self, abs: &Path, bytes: &[u8]) {
            self.files.insert(abs.to_path_buf(), bytes.to_vec());
        }
        fn with_tree(&mut self, root: &Path, files: &[&str]) {
            let abs: Vec<PathBuf> = files.iter().map(|f| root.join(f)).collect();
            self.trees
                .lock()
                .expect("lock")
                .insert(root.to_path_buf(), abs);
        }
    }

    #[async_trait::async_trait]
    impl WorkspaceFilesPort for FakeFiles {
        async fn read(&self, path: &Path) -> Option<String> {
            String::from_utf8(self.read_bytes(path).await?).ok()
        }
        async fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
            self.files.get(path).cloned()
        }
        async fn write(&self, _p: &Path, _c: &str) -> bool {
            false
        }
        async fn write_bytes(&self, _p: &Path, _b: &[u8]) -> bool {
            false
        }
        async fn delete(&self, _p: &Path) -> bool {
            false
        }
        async fn stat(&self, _p: &Path) -> Option<crate::ports::outbound::FileMeta> {
            None
        }
        async fn list_recursive(&self, dir: &Path) -> Vec<PathBuf> {
            self.trees
                .lock()
                .expect("lock")
                .get(dir)
                .cloned()
                .unwrap_or_default()
        }
        async fn list_dirs(&self, _dir: &Path) -> Vec<PathBuf> {
            Vec::new()
        }
        async fn list(&self, _dir: &Path) -> Vec<crate::ports::outbound::FileMeta> {
            Vec::new()
        }
    }

    #[derive(Default)]
    struct FakeArchive {
        saved: Mutex<Vec<(WorkspaceArchive, PathBuf)>>,
        locked: Mutex<std::collections::HashSet<PathBuf>>,
    }

    #[async_trait::async_trait]
    impl BackupArchivePort for FakeArchive {
        async fn save(
            &self,
            archive: &WorkspaceArchive,
            path: &Path,
        ) -> Result<(), crate::PortError> {
            self.saved
                .lock()
                .expect("lock")
                .push((archive.clone(), path.to_path_buf()));
            Ok(())
        }
        async fn load(&self, _path: &Path) -> Result<WorkspaceArchive, crate::PortError> {
            Err(crate::PortError::NotFound("unused in backup tests".into()))
        }
        async fn probe_locked(&self, lock_path: &Path) -> bool {
            self.locked.lock().expect("lock").contains(lock_path)
        }
    }

    // --- fixture ------------------------------------------------------------

    const HUB: &str = "/tmp-hub";
    const PROJECT: &str = "cxa";

    fn registry_entry(path: &str) -> String {
        format!(r#"[{{ "id": "{PROJECT}", "path": "{path}" }}]"#)
    }

    fn setup(files: &mut FakeFiles, project_path: &str) {
        files.with_file(
            &Path::new(HUB).join("registry.json"),
            registry_entry(project_path).as_bytes(),
        );
        files.with_file(
            &Path::new(HUB).join(PROJECT).join("coxagent.json"),
            br#"{"deploy":{"host_port":null}}"#,
        );
        files.with_file(
            &Path::new(HUB).join(PROJECT).join("state/state.json"),
            br#"{"schema_version":4,"tickets":[]}"#,
        );
        files.with_file(
            &Path::new(HUB).join(PROJECT).join("state/.state.lock"),
            b"lockfile",
        );
        files.with_file(&Path::new(HUB).join("sessions.json"), b"live tokens");
        files.with_file(&Path::new(HUB).join("auth.json"), br#"{"users":{}}"#);
        files.with_tree(
            &Path::new(HUB).join(PROJECT).join("state"),
            &["state.json", ".state.lock", ".backups/state.json.1"],
        );
        files.with_tree(&Path::new(HUB).join("blobs"), &["evidence.png"]);
        files.with_file(&Path::new(HUB).join("blobs/evidence.png"), b"png-bytes");
    }

    fn request(out: &str) -> BackupRequest {
        BackupRequest {
            hub_dir: PathBuf::from(HUB),
            out: PathBuf::from(out),
            secrets_root: None,
        }
    }

    async fn run(files: &FakeFiles, req: &BackupRequest) -> BackupOutcome {
        BackupWorkspaceUseCase::new(
            Arc::new(Clone::clone(files)),
            Arc::new(FakeArchive::default()),
        )
        .execute(req)
        .await
        .expect("backup succeeds")
    }

    // --- spec inclusion/exclusion rules --------------------------------------

    #[tokio::test]
    async fn captures_registry_config_state_and_blobs_but_never_secrets_or_locks() {
        let mut files = FakeFiles::default();
        let proj = format!("{HUB}/{PROJECT}");
        setup(&mut files, &proj);
        let uc = BackupWorkspaceUseCase::new(Arc::new(files), Arc::new(FakeArchive::default()));
        let out = uc
            .execute(&request("/out/archive.hubarchive.json"))
            .await
            .expect("backup");

        assert_eq!(out.manifest.archive_schema, ARCHIVE_SCHEMA_VERSION);
        let names: Vec<&str> = out
            .captured
            .iter()
            .map(|p| p.strip_prefix("workspace/").unwrap_or(p))
            .collect();
        for expected in [
            "registry.json",
            "auth.json",
            "cxa/coxagent.json",
            "cxa/state/state.json",
            "blobs/evidence.png",
        ] {
            assert!(
                names.contains(&expected),
                "must capture {expected}: {names:?}"
            );
        }
        for forbidden in [
            "cxa/state/.state.lock",
            "sessions.json",
            "cxa/state/.backups/state.json.1",
        ] {
            assert!(
                !names.contains(&forbidden),
                "must never capture {forbidden}"
            );
        }
        assert_eq!(out.manifest.session_tokens, SecretChoice::Excluded);
        assert_eq!(out.manifest.deploy_secrets, SecretChoice::Excluded);
    }

    #[tokio::test]
    async fn a_project_outside_the_hub_dir_is_skipped_with_a_warning() {
        let mut files = FakeFiles::default();
        setup(&mut files, "/elsewhere/cxa");
        let out = run(&files, &request("/out/a.hubarchive.json")).await;
        assert!(
            !out.captured.iter().any(|p| p.contains("coxagent.json")),
            "a non-portable project must not be captured"
        );
        assert!(
            out.warnings.iter().any(|w| w.contains("/elsewhere/cxa")),
            "the operator must be told why: {:?}",
            out.warnings
        );
    }

    #[tokio::test]
    async fn opt_in_secrets_travel_under_the_secrets_root_and_state_the_choice() {
        let mut files = FakeFiles::default();
        let proj = format!("{HUB}/{PROJECT}");
        setup(&mut files, &proj);
        files.with_tree(Path::new("/machine-secrets"), &["proj-name.env"]);
        files.with_file(Path::new("/machine-secrets/proj-name.env"), b"PGPASSWORD=x");
        let uc = BackupWorkspaceUseCase::new(Arc::new(files), Arc::new(FakeArchive::default()));
        let out = uc
            .execute(&BackupRequest {
                hub_dir: PathBuf::from(HUB),
                out: PathBuf::from("/out/a.hubarchive.json"),
                secrets_root: Some(PathBuf::from("/machine-secrets")),
            })
            .await
            .expect("backup");

        assert_eq!(out.manifest.deploy_secrets, SecretChoice::Included);
        assert!(
            out.captured.iter().any(|p| p == "secrets/proj-name.env"),
            "{:?}",
            out.captured
        );
        assert!(out.secret_statement.contains("INCLUDED"));
        assert!(out.secret_statement.contains("sessions.json"));
    }

    // --- manifest seal + torn defence ----------------------------------------

    #[tokio::test]
    async fn a_saved_archive_validates_end_to_end() {
        let mut files = FakeFiles::default();
        let proj = format!("{HUB}/{PROJECT}");
        setup(&mut files, &proj);
        let saved = Arc::new(FakeArchive::default());
        let uc = BackupWorkspaceUseCase::new(Arc::new(files), saved.clone());
        uc.execute(&request("/out/a.hubarchive.json"))
            .await
            .expect("backup");
        let (archive, _path) = &saved.saved.lock().expect("lock")[0];

        validate_archive(archive).expect("the produced archive must pass restore-side validation");
        assert_eq!(archive.manifest.file_count, archive.files.len());
        assert_eq!(
            archive.manifest.total_bytes,
            archive
                .files
                .iter()
                .map(
                    |f| u64::try_from(hex_decode(&f.bytes_hex).expect("hex").len())
                        .unwrap_or(u64::MAX)
                )
                .sum::<u64>()
        );
        for f in &archive.files {
            assert_eq!(
                sha256_hex(&hex_decode(&f.bytes_hex).expect("hex")),
                f.sha256
            );
        }
    }

    #[tokio::test]
    async fn a_torn_json_file_is_never_archived() {
        let mut files = FakeFiles::default();
        let proj = format!("{HUB}/{PROJECT}");
        setup(&mut files, &proj);
        files.with_file(&Path::new(HUB).join("auth.json"), b"{'torn': ".as_slice());
        let err = BackupWorkspaceUseCase::new(Arc::new(files), Arc::new(FakeArchive::default()))
            .execute(&request("/out/a.hubarchive.json"))
            .await
            .expect_err("torn json must fail the backup loudly");
        assert!(err.to_string().contains("not valid JSON"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_registry_fails_the_backup_with_direction() {
        let files = FakeFiles::default();
        let err = BackupWorkspaceUseCase::new(Arc::new(files), Arc::new(FakeArchive::default()))
            .execute(&request("/out/a.hubarchive.json"))
            .await
            .expect_err("no registry");
        assert!(err.to_string().contains("registry"), "{err}");
    }

    // --- pure exclusion rules -------------------------------------------------

    #[test]
    fn the_exclusion_list_never_nests_backups_or_pre_restore_snapshots() {
        for excluded in [
            "backups/coxagent-backup-1.hubarchive.json",
            ".pre-restore-20260901T00/registry.json",
            "cxa/state/.backups/state.json.1",
            "cxa/state/.state.lock",
            "cxa/state/.state.tmp.42",
            // A stray `--out` inside a project tree must never nest archives.
            "cxa/state/coxagent-backup-1.hubarchive.json",
            "sessions.json",
            "operator.token",
        ] {
            assert!(is_excluded(excluded), "{excluded} must be excluded");
        }
        for captured in [
            "registry.json",
            "coordination.json",
            "cxa/coxagent.json",
            "cxa/state/state.json",
            "blobs/evidence.png",
        ] {
            assert!(!is_excluded(captured), "{captured} must be captured");
        }
    }

    #[test]
    fn rel_paths_are_forward_slash_and_root_anchored() {
        assert_eq!(
            rel_from(Path::new("/hub"), Path::new("/hub/cxa/state/state.json")),
            "cxa/state/state.json"
        );
        assert_eq!(rel_from(Path::new("/hub"), Path::new("/elsewhere/x")), "");
    }

    #[test]
    fn the_path_stamp_is_filesystem_safe_and_sorts_chronologically() {
        let a = path_stamp("2026-09-01T00:00:00Z");
        let b = path_stamp("2026-09-01T01:00:00Z");
        assert_eq!(a, "20260901T000000Z");
        assert!(a < b, "lexical order is chronological");
        assert!(!a.contains(':') && !a.contains('-'));
    }
}
