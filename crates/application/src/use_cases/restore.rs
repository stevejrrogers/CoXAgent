//! `RestoreWorkspaceUseCase` — one command to rebuild a hub workspace from a
//! CXA-F262 archive, refusing every way a restore could destroy state.
//!
//! The order of the refusals IS the design: everything that can reject the
//! archive is checked BEFORE anything is written, and in this order —
//!
//! 1. schema ratchet: an archive written by a newer binary is refused with a
//!    clear error (the archive-level twin of the store's `parse_checked`);
//! 2. integrity: the manifest seal and every per-file seal must verify — a
//!    tampered or truncated archive is refused;
//! 3. path safety: no `..`, no absolute paths, no empty segments (see
//!    `is_safe_archive_path`) — a hostile archive can never escape the
//!    restore root;
//! 4. a live hub: any project state dir the plan touches whose `.state.lock`
//!    is held by another process refuses the restore mechanically, instead
//!    of trusting the operator to have stopped the hub;
//! 5. a non-empty target: without `--force`, an existing workspace is never
//!    touched; with `--force`, the workspace files about to be overwritten
//!    are snapshotted into `<hub-dir>/.pre-restore-<ts>/` first — the safety
//!    snapshot captures exactly the overwritten workspace set, so a bad
//!    restore is itself restorable. Overwritten deploy secrets are NOT
//!    copied into the snapshot (a plaintext credential copy outside the
//!    secrets root is the hygiene hole CXA-B031/B036 exist for); the
//!    overwrite count is stated in the summary instead.
//!
//! `--dry-run` runs steps 1–5 and the plan, then writes NOTHING — provably,
//! by construction: the write loop is below the dry-run return.
//!
//! Known limitation (inherent to the registry format, not the archive): hub
//! registry entries carry ABSOLUTE project paths, so an archive moved to a
//! new machine restores byte-faithfully but its registry.json still points
//! at the source machine's dirs — the operator points those entries at the
//! restored locations (a one-file edit) before booting the hub there. The
//! restore never rewrites captured bytes.

use super::backup::{is_excluded, path_stamp, rel_from};
use crate::config_parse::parse_config;
use crate::ports::outbound::{
    hex_decode, validate_archive, ArchiveFile, ArchiveRoot, BackupArchivePort, WorkspaceFilesPort,
};
use crate::state::now_rfc3339;
use crate::AppError;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where and how to restore.
#[derive(Debug, Clone)]
pub struct RestoreRequest {
    /// The hub directory to rebuild.
    pub hub_dir: PathBuf,
    /// The `.hubarchive.json` file to restore from.
    pub archive_path: PathBuf,
    /// The CURRENT machine's deploy-secrets root — secrets always restore
    /// here, never to the source machine's path.
    pub secrets_root: PathBuf,
    /// Overwrite a non-empty target (a pre-restore snapshot is taken first).
    pub force: bool,
    /// Verify and plan without writing anything.
    pub dry_run: bool,
}

/// What one restore did (or would do, under `--dry-run`).
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    /// Workspace files written (empty under `--dry-run`).
    pub restored: Vec<String>,
    /// Secret files written to the current machine's deploy-secrets root.
    pub secrets_restored: Vec<String>,
    /// The safety snapshot directory holding the previous bytes of every
    /// overwritten file, when any file was overwritten.
    pub snapshot_dir: Option<String>,
    /// Re-clone hints: the archive carries the hub's STATE, not the managed
    /// codebases — a restored project whose `codebase/` is empty needs its
    /// git repo cloned back (derived from coxagent.json's git.repo).
    pub hints: Vec<String>,
    pub summary: String,
}

/// One planned write: the archive file it came from, its absolute target on
/// THIS machine, and its decoded payload.
type RestoreTarget<'a> = (&'a ArchiveFile, PathBuf, Vec<u8>);

/// The write plan: every archive file resolved onto this machine's
/// filesystem (workspace files under the target hub dir, secrets under the
/// CURRENT machine's secrets root), payloads decoded.
///
/// `validate_archive` has already decoded and hashed every file, so the
/// decode cannot fail — but fail closed rather than ever write empty bytes
/// in place of a payload.
fn plan_targets<'a>(
    req: &RestoreRequest,
    archive: &'a crate::ports::outbound::WorkspaceArchive,
) -> Result<Vec<RestoreTarget<'a>>, AppError> {
    archive
        .files
        .iter()
        .map(
            |f: &'a ArchiveFile| -> Result<RestoreTarget<'a>, AppError> {
                let root = match f.root {
                    ArchiveRoot::Workspace => req.hub_dir.clone(),
                    ArchiveRoot::Secrets => req.secrets_root.clone(),
                };
                let bytes = hex_decode(&f.bytes_hex).ok_or_else(|| {
                    AppError::Port(crate::PortError::Corrupt(format!(
                        "archive file {:?} has malformed hex payload — refusing before                          any write",
                        f.path
                    )))
                })?;
                Ok((f, root.join(&f.path), bytes))
            },
        )
        .collect()
}

/// Rebuild a hub workspace from an archive.
pub struct RestoreWorkspaceUseCase<W: WorkspaceFilesPort, A: BackupArchivePort> {
    files: Arc<W>,
    archive: Arc<A>,
}

impl<W: WorkspaceFilesPort, A: BackupArchivePort> RestoreWorkspaceUseCase<W, A> {
    pub fn new(files: Arc<W>, archive: Arc<A>) -> Self {
        Self { files, archive }
    }

    /// # Errors
    /// [`AppError`] when the archive is unreadable, from a newer schema,
    /// tampered, carries unsafe paths, targets a hub that is running, or
    /// would overwrite a non-empty workspace without `--force`.
    pub async fn execute(&self, req: &RestoreRequest) -> Result<RestoreOutcome, AppError> {
        let archive = self.archive.load(&req.archive_path).await?;
        // Everything below runs BEFORE any write — validate_archive alone
        // covers schema, both integrity seals and path safety.
        validate_archive(&archive)?;
        let plan = plan_targets(req, &archive)?;

        self.refuse_running_hub(&plan).await?;

        let existing = self.existing_targets(&req.hub_dir).await;
        if !existing.is_empty() && !req.force {
            return Err(AppError::Port(crate::PortError::Conflict(format!(
                "target hub dir {} is not empty ({} file(s) under it) — restoring would \
                 overwrite existing state; pass --force to snapshot and overwrite, or \
                 restore into an empty directory",
                req.hub_dir.display(),
                existing.len()
            ))));
        }

        if req.dry_run {
            return Ok(RestoreOutcome {
                restored: Vec::new(),
                secrets_restored: Vec::new(),
                snapshot_dir: None,
                hints: self.reclone_hints(&plan).await,
                summary: format!(
                    "dry-run: archive is valid, {} file(s) would be written, target has {} \
                     existing file(s); NO changes were written",
                    plan.len(),
                    existing.len()
                ),
            });
        }

        // Safety first: snapshot exactly the workspace files about to be
        // overwritten, keeping the archive's own relative layout. Deploy
        // secrets are deliberately NOT snapshotted — see snapshot_overwritten.
        let snapshot_dir = req
            .hub_dir
            .join(format!(".pre-restore-{}", path_stamp(&now_rfc3339())));
        let (overwritten, secrets_overwritten) =
            self.snapshot_overwritten(&plan, &snapshot_dir).await?;

        let mut restored = Vec::new();
        let mut secrets_restored = Vec::new();
        for (f, abs, bytes) in &plan {
            if !self.files.write_bytes(abs, bytes).await {
                return Err(AppError::Port(crate::PortError::Backend(format!(
                    "cannot write {} — restore aborted mid-way; the pre-restore snapshot \
                     at {} holds the previous bytes",
                    abs.display(),
                    snapshot_dir.display()
                ))));
            }
            match f.root {
                ArchiveRoot::Workspace => restored.push(format!("workspace/{}", f.path)),
                ArchiveRoot::Secrets => {
                    secrets_restored.push(format!("secrets/{}", f.path));
                }
            }
        }

        let hints = self.reclone_hints(&plan).await;
        let snapshot = if overwritten > 0 {
            Some(snapshot_dir.display().to_string())
        } else {
            None
        };
        let secrets_note = if secrets_overwritten > 0 {
            format!(
                "; {secrets_overwritten} overwritten secret file(s) were NOT snapshotted — \
                 they are this machine's own deploy secrets and regenerate on the next \
                 deploy",
            )
        } else {
            String::new()
        };
        let summary = format!(
            "restored {} file(s) into {} ({} overwritten; deploy secrets went to the \
             CURRENT machine's root {}){}{}",
            plan.len(),
            req.hub_dir.display(),
            overwritten,
            req.secrets_root.display(),
            snapshot
                .as_ref()
                .map(|s| format!(" — previous bytes: {s}"))
                .unwrap_or_default(),
            secrets_note,
        );
        Ok(RestoreOutcome {
            restored,
            secrets_restored,
            snapshot_dir: snapshot,
            hints,
            summary,
        })
    }

    /// Copy the current bytes of every WORKSPACE file the plan will overwrite
    /// into the pre-restore snapshot, preserving the archive's relative
    /// layout. Returns `(workspace files snapshotted, secret files
    /// overwritten)`.
    ///
    /// Deploy secrets are deliberately NOT copied into the snapshot: the
    /// snapshot lives inside the hub dir, and a plaintext credential copy
    /// outside the secrets root — at default file permissions, under a path
    /// the regeneration mechanism knows nothing about — is exactly the
    /// hygiene hole CXA-B031/B036 exist for. Overwritten secret files are the
    /// CURRENT machine's own deploy secrets, which regenerate on the next
    /// deploy; the count is reported so the summary can say so.
    async fn snapshot_overwritten(
        &self,
        plan: &[RestoreTarget<'_>],
        snapshot_dir: &Path,
    ) -> Result<(usize, usize), AppError> {
        let mut snapshotted = 0usize;
        let mut secrets_overwritten = 0usize;
        for (f, abs, _) in plan {
            let Some(old) = self.files.read_bytes(abs).await else {
                continue; // nothing to overwrite — nothing to snapshot
            };
            if f.root == ArchiveRoot::Secrets {
                secrets_overwritten += 1;
                continue;
            }
            let snap_path = snapshot_dir.join(f.root.tag()).join(&f.path);
            if !self.files.write_bytes(&snap_path, &old).await {
                return Err(AppError::Port(crate::PortError::Backend(format!(
                    "cannot write the pre-restore snapshot at {} — refusing to \
                     overwrite without a safety copy",
                    snap_path.display()
                ))));
            }
            snapshotted += 1;
        }
        Ok((snapshotted, secrets_overwritten))
    }

    /// Mechanically refuse when any project state dir the plan touches is
    /// locked by another process (a hub writing while we overwrite it).
    async fn refuse_running_hub(&self, plan: &[RestoreTarget<'_>]) -> Result<(), AppError> {
        let mut locks: BTreeSet<PathBuf> = BTreeSet::new();
        for (f, abs, _) in plan {
            if f.root != ArchiveRoot::Workspace {
                continue;
            }
            // Every ancestor directory named `state` is a JsonStateStore root
            // that holds `.state.lock` while writing.
            for dir in abs.ancestors().skip(1) {
                if dir.file_name().is_some_and(|n| n == "state") {
                    locks.insert(dir.join(".state.lock"));
                }
            }
        }
        for lock in locks {
            if self.archive.probe_locked(&lock).await {
                return Err(AppError::Port(crate::PortError::Conflict(format!(
                    "{} is held by another process — a hub appears to be running; stop it \
                     before restoring",
                    lock.display()
                ))));
            }
        }
        Ok(())
    }

    /// Files already present under the hub dir, as archive-relative paths —
    /// ignoring the archive store itself and previous pre-restore snapshots,
    /// which are backup machinery, not workspace state.
    async fn existing_targets(&self, hub_dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for abs in self.files.list_recursive(hub_dir).await {
            let rel = rel_from(hub_dir, &abs);
            if rel.is_empty() || is_excluded(&rel) {
                continue;
            }
            out.push(rel);
        }
        out
    }

    /// Re-clone hints for restored projects whose managed codebase is not
    /// there: the archive deliberately carries state, not the codebase.
    async fn reclone_hints(&self, plan: &[RestoreTarget<'_>]) -> Vec<String> {
        let mut hints = Vec::new();
        for (f, abs, bytes) in plan {
            if f.root != ArchiveRoot::Workspace || !f.path.ends_with("/coxagent.json") {
                continue;
            }
            let Ok(text) = std::str::from_utf8(bytes) else {
                continue;
            };
            let Ok(config) = parse_config(text) else {
                continue;
            };
            if config.git.repo.is_empty() {
                continue;
            }
            let id = abs
                .parent()
                .and_then(Path::file_name)
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let codebase = abs
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf)
                .join("codebase");
            let missing = self.files.list_recursive(&codebase).await.is_empty();
            if missing {
                hints.push(format!(
                    "project '{id}': the archive carries its state, not its codebase — \
                     clone {} into {} (or copy the checkout over)",
                    config.git.repo,
                    codebase.display()
                ));
            }
        }
        hints
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{
        archive_sha256, hex_encode, ArchiveManifest, SecretChoice, WorkspaceArchive,
        ARCHIVE_SCHEMA_VERSION,
    };
    use crate::PortError;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    // --- in-memory doubles (the MemStore pattern) -----------------------------
    //
    // The ports take `&self`, so anything the restore WRITES sits behind a
    // mutex — the same interior mutability every in-memory double here uses.

    #[derive(Default, Clone)]
    struct FakeFiles {
        files: Arc<Mutex<BTreeMap<PathBuf, Vec<u8>>>>,
        trees: Arc<Mutex<BTreeMap<PathBuf, Vec<PathBuf>>>>,
    }

    impl FakeFiles {
        fn with_tree(&self, root: &Path, files: &[&str]) {
            self.trees.lock().expect("lock").insert(
                root.to_path_buf(),
                files.iter().map(|f| root.join(f)).collect(),
            );
        }
        fn put(&self, path: &Path, bytes: &[u8]) {
            self.files
                .lock()
                .expect("lock")
                .insert(path.to_path_buf(), bytes.to_vec());
        }
        fn file_names(&self) -> Vec<String> {
            self.files
                .lock()
                .expect("lock")
                .keys()
                .map(|p| p.display().to_string())
                .collect()
        }
        fn bytes_of(&self, path: &Path) -> Option<Vec<u8>> {
            self.files.lock().expect("lock").get(path).cloned()
        }
        fn snapshot_registry_bytes(&self) -> Option<Vec<u8>> {
            self.files
                .lock()
                .expect("lock")
                .iter()
                .find(|(p, _)| {
                    p.display().to_string().contains(".pre-restore-")
                        && p.ends_with("registry.json")
                })
                .map(|(_, b)| b.clone())
        }
    }

    #[async_trait::async_trait]
    impl WorkspaceFilesPort for FakeFiles {
        async fn read(&self, path: &Path) -> Option<String> {
            String::from_utf8(self.read_bytes(path).await?).ok()
        }
        async fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
            self.files.lock().expect("lock").get(path).cloned()
        }
        async fn write(&self, _p: &Path, _c: &str) -> bool {
            false
        }
        async fn write_bytes(&self, path: &Path, bytes: &[u8]) -> bool {
            self.files
                .lock()
                .expect("lock")
                .insert(path.to_path_buf(), bytes.to_vec());
            true
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

    #[derive(Default, Clone)]
    struct FakeArchive {
        stored: BTreeMap<PathBuf, WorkspaceArchive>,
        locked: BTreeSet<PathBuf>,
    }

    #[async_trait::async_trait]
    impl BackupArchivePort for FakeArchive {
        async fn save(&self, _a: &WorkspaceArchive, _p: &Path) -> Result<(), PortError> {
            Err(PortError::Backend("unused in restore tests".into()))
        }
        async fn load(&self, path: &Path) -> Result<WorkspaceArchive, PortError> {
            self.stored
                .get(path)
                .cloned()
                .ok_or_else(|| PortError::NotFound(path.display().to_string()))
        }
        async fn probe_locked(&self, lock_path: &Path) -> bool {
            self.locked.contains(lock_path)
        }
    }

    // --- archive fixture builder ----------------------------------------------

    const HUB: &str = "/restore-hub";
    const ARCHIVE: &str = "/archives/a.hubarchive.json";

    fn file(root: ArchiveRoot, path: &str, bytes: &[u8]) -> ArchiveFile {
        ArchiveFile {
            root,
            path: path.to_owned(),
            bytes_hex: hex_encode(bytes),
            sha256: crate::ports::outbound::sha256_hex(bytes),
        }
    }

    fn manifest(files: &[ArchiveFile]) -> ArchiveManifest {
        ArchiveManifest {
            archive_schema: ARCHIVE_SCHEMA_VERSION,
            created_at: "2026-09-01T00:00:00Z".to_owned(),
            file_count: files.len(),
            total_bytes: files
                .iter()
                .map(|f| u64::try_from(f.bytes_hex.len() / 2).unwrap_or(u64::MAX))
                .sum(),
            sha256: archive_sha256(files),
            session_tokens: SecretChoice::Excluded,
            deploy_secrets: SecretChoice::Excluded,
        }
    }

    fn with_archive(archive_port: &mut FakeArchive, files: Vec<ArchiveFile>) {
        let archive = WorkspaceArchive {
            manifest: manifest(&files),
            files,
        };
        archive_port.stored.insert(PathBuf::from(ARCHIVE), archive);
    }

    fn request(force: bool, dry_run: bool) -> RestoreRequest {
        RestoreRequest {
            hub_dir: PathBuf::from(HUB),
            archive_path: PathBuf::from(ARCHIVE),
            secrets_root: PathBuf::from("/machine-secrets"),
            force,
            dry_run,
        }
    }

    async fn restore(
        files: &FakeFiles,
        archive_port: &FakeArchive,
        req: &RestoreRequest,
    ) -> Result<RestoreOutcome, AppError> {
        RestoreWorkspaceUseCase::new(Arc::new(files.clone()), Arc::new(archive_port.clone()))
            .execute(req)
            .await
    }

    const REGISTRY: &[u8] = br#"[{ "id": "cxa", "path": "/restore-hub/cxa" }]"#;
    const CONFIG: &[u8] = br#"{"git":{"enabled":true,"repo":"git@github.com:acme/cxa.git"}}"#;
    const STATE: &[u8] = br#"{"schema_version":4,"tickets":[]}"#;

    fn full_plan() -> Vec<ArchiveFile> {
        vec![
            file(ArchiveRoot::Workspace, "registry.json", REGISTRY),
            file(ArchiveRoot::Workspace, "cxa/coxagent.json", CONFIG),
            file(ArchiveRoot::Workspace, "cxa/state/state.json", STATE),
            file(ArchiveRoot::Workspace, "blobs/evidence.png", b"png-bytes"),
        ]
    }

    // --- the refusal order: schema, integrity, safety — before ANY write ------

    #[tokio::test]
    async fn a_newer_schema_is_refused_with_a_clear_error_before_any_write() {
        let files = FakeFiles::default();
        let mut archive_port = FakeArchive::default();
        let mut archive = WorkspaceArchive {
            manifest: manifest(&full_plan()),
            files: full_plan(),
        };
        archive.manifest.archive_schema = ARCHIVE_SCHEMA_VERSION + 1;
        archive_port.stored.insert(PathBuf::from(ARCHIVE), archive);

        let err = restore(&files, &archive_port, &request(false, false))
            .await
            .expect_err("a newer schema must be refused");
        assert!(err.to_string().contains("newer"), "{err}");
        assert!(
            files.file_names().is_empty(),
            "the refusal must happen before any write"
        );
    }

    #[tokio::test]
    async fn tampered_bytes_are_refused_before_any_write() {
        let files = FakeFiles::default();
        let mut archive_port = FakeArchive::default();
        let mut plan = full_plan();
        plan[2].bytes_hex = hex_encode(br#"{"schema_version":9,"tickets":[],"hacked":true}"#);
        with_archive(&mut archive_port, plan);

        let err = restore(&files, &archive_port, &request(false, false))
            .await
            .expect_err("tampered bytes must be refused");
        assert!(err.to_string().contains("integrity"), "{err}");
        assert!(files.file_names().is_empty(), "nothing written on refusal");
    }

    #[tokio::test]
    async fn unsafe_paths_are_refused_before_any_write() {
        for hostile in [
            "../../etc/passwd",
            "/etc/passwd",
            "a//b",
            "./x",
            // `\` is a separator on Windows: `a\..\..\x` would traverse there.
            "a\\..\\..\\x",
        ] {
            let files = FakeFiles::default();
            let mut archive_port = FakeArchive::default();
            let plan = vec![
                file(ArchiveRoot::Workspace, "registry.json", REGISTRY),
                file(ArchiveRoot::Workspace, hostile, b"evil"),
            ];
            with_archive(&mut archive_port, plan);

            let err = restore(&files, &archive_port, &request(false, false))
                .await
                .expect_err("an unsafe path must be refused");
            assert!(err.to_string().contains("unsafe path"), "{hostile}: {err}");
            assert!(files.file_names().is_empty());
        }
    }

    // --- the running-hub refusal ------------------------------------------------

    #[tokio::test]
    async fn a_locked_project_state_refuses_the_restore_mechanically() {
        let files = FakeFiles::default();
        let mut archive_port = FakeArchive::default();
        with_archive(&mut archive_port, full_plan());
        archive_port
            .locked
            .insert(PathBuf::from(HUB).join("cxa/state/.state.lock"));

        let err = restore(&files, &archive_port, &request(false, false))
            .await
            .expect_err("a running hub must be refused");
        assert!(err.to_string().contains("running"), "{err}");
        assert!(files.file_names().is_empty());
    }

    // --- the non-empty refusal + force + snapshot -------------------------------

    #[tokio::test]
    async fn a_non_empty_target_is_refused_unless_forced() {
        let files = FakeFiles::default();
        let mut archive_port = FakeArchive::default();
        with_archive(&mut archive_port, full_plan());
        files.with_tree(Path::new(HUB), &["registry.json"]);
        // Mark the listed file as existing content too.
        files.put(&PathBuf::from(HUB).join("registry.json"), b"old registry");

        let err = restore(&files, &archive_port, &request(false, false))
            .await
            .expect_err("non-empty target must be refused without force");
        assert!(err.to_string().contains("not empty"), "{err}");

        let out = restore(&files, &archive_port, &request(true, false))
            .await
            .expect("force overwrites");
        assert_eq!(out.restored.len(), 4);
        assert!(out.snapshot_dir.is_some(), "force must snapshot first");
        // The overwritten file's previous bytes live in the snapshot.
        assert_eq!(
            files.snapshot_registry_bytes().expect("snapshot bytes"),
            b"old registry".to_vec()
        );
    }

    /// Workspace state gets the safety snapshot; overwritten deploy secrets
    /// do NOT (a plaintext credential copy inside the hub dir is the hygiene
    /// hole CXA-B031/B036 exist for) — but the overwrite is counted and
    /// stated in the summary.
    #[tokio::test]
    async fn overwritten_secrets_are_counted_but_never_snapshotted() {
        let files = FakeFiles::default();
        let mut archive_port = FakeArchive::default();
        let plan = vec![
            file(ArchiveRoot::Workspace, "registry.json", REGISTRY),
            file(ArchiveRoot::Secrets, "demo.env", b"PGPASSWORD=new"),
        ];
        with_archive(&mut archive_port, plan);
        // The current machine already holds a DIFFERENT value for this secret.
        files.with_tree(Path::new("/machine-secrets"), &["demo.env"]);
        files.put(
            &PathBuf::from("/machine-secrets/demo.env"),
            b"PGPASSWORD=old",
        );

        let out = restore(&files, &archive_port, &request(true, false))
            .await
            .expect("force restores");
        assert_eq!(out.snapshot_dir, None, "no workspace file was overwritten");
        assert!(
            !files
                .file_names()
                .iter()
                .any(|p| p.contains(".pre-restore-")),
            "secret bytes must never be copied into the hub dir"
        );
        assert_eq!(
            files.bytes_of(&PathBuf::from("/machine-secrets/demo.env")),
            Some(b"PGPASSWORD=new".to_vec()),
            "the restore itself still lands"
        );
        assert!(
            out.summary.contains("1 overwritten secret file(s)"),
            "the operator must be told: {}",
            out.summary
        );
    }

    #[tokio::test]
    async fn restore_into_an_empty_target_needs_no_force() {
        let files = FakeFiles::default();
        let mut archive_port = FakeArchive::default();
        with_archive(&mut archive_port, full_plan());

        let out = restore(&files, &archive_port, &request(false, false))
            .await
            .expect("empty target restores without force");
        assert_eq!(out.restored.len(), 4);
        assert!(out.snapshot_dir.is_none(), "nothing was overwritten");
        assert_eq!(
            files.bytes_of(&PathBuf::from(HUB).join("registry.json")),
            Some(REGISTRY.to_vec())
        );
    }

    // --- dry-run provably mutates nothing ---------------------------------------

    #[tokio::test]
    async fn dry_run_reports_the_plan_and_writes_nothing() {
        let files = FakeFiles::default();
        let mut archive_port = FakeArchive::default();
        with_archive(&mut archive_port, full_plan());

        let out = restore(&files, &archive_port, &request(false, true))
            .await
            .expect("dry-run succeeds");
        assert!(out.restored.is_empty());
        assert!(out.snapshot_dir.is_none());
        assert!(files.file_names().is_empty(), "dry-run must write nothing");
        assert!(out.summary.contains("NO changes were written"));

        // On a NON-EMPTY target the dry-run surfaces the same refusal the
        // real restore would produce — as an error, still writing nothing.
        let occupied = FakeFiles::default();
        occupied.with_tree(Path::new(HUB), &["registry.json"]);
        let err = restore(&occupied, &archive_port, &request(false, true))
            .await
            .expect_err("dry-run must surface the non-empty refusal");
        assert!(err.to_string().contains("not empty"), "{err}");
        assert!(
            occupied.file_names().is_empty(),
            "even a refusing dry-run writes nothing"
        );
    }

    // --- secrets restore to the CURRENT machine's root ---------------------------

    #[tokio::test]
    async fn secrets_restore_under_the_current_machines_secrets_root() {
        let files = FakeFiles::default();
        let mut archive_port = FakeArchive::default();
        let plan = vec![
            file(ArchiveRoot::Workspace, "registry.json", REGISTRY),
            file(ArchiveRoot::Secrets, "cxa.env", b"PGPASSWORD=x"),
        ];
        with_archive(&mut archive_port, plan);

        let out = restore(&files, &archive_port, &request(false, false))
            .await
            .expect("restore");
        assert_eq!(out.secrets_restored, vec!["secrets/cxa.env"]);
        assert_eq!(
            files
                .bytes_of(&PathBuf::from("/machine-secrets/cxa.env"))
                .expect("secret on the current machine"),
            b"PGPASSWORD=x".to_vec()
        );
        assert!(
            files
                .bytes_of(&PathBuf::from(HUB).join("cxa.env"))
                .is_none(),
            "the secret must never land inside the hub dir"
        );
    }

    // --- re-clone hints -----------------------------------------------------------

    #[tokio::test]
    async fn a_restored_project_with_a_repo_but_no_codebase_gets_a_reclone_hint() {
        let files = FakeFiles::default();
        let mut archive_port = FakeArchive::default();
        with_archive(&mut archive_port, full_plan());

        let out = restore(&files, &archive_port, &request(false, false))
            .await
            .expect("restore");
        assert_eq!(out.hints.len(), 1, "{:?}", out.hints);
        assert!(
            out.hints[0].contains("git@github.com:acme/cxa.git"),
            "{}",
            out.hints[0]
        );
        assert!(out.hints[0].contains("codebase"), "{}", out.hints[0]);
    }
}
