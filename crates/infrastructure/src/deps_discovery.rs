//! `FsLockfileDiscovery` — the [`DependencyDiscoveryPort`] adapter over the
//! real filesystem (CXA-B111): walks a workspace root, skips the directories
//! that can never carry a scannable lock, and reads every file the scanner's
//! own contract recognises. Until this shipped, discovery existed only as
//! test-only IO inside the F009 gate and the scanner had no production caller.
//!
//! CXA-B118 walk policy — the walk is bounded and never leaves `root`:
//! classification reads the directory entry's OWN file type, so a symlinked
//! directory (e.g. a DMG staging `Applications -> /Applications`) is never
//! descended into and a symlink is never read through. `Path::is_dir` follows
//! both, which once sent the walk 332k files deep into Xcode.app and wedged
//! the scan endpoint. A workspace that still exceeds the visit bound fails
//! the scan with an error instead of hanging its request.

use async_trait::async_trait;
use coxagent_application::deps_scan;
use coxagent_application::ports::outbound::{DependencyDiscoveryPort, Lockfile};
use coxagent_application::PortError;
use std::path::Path;

/// Directories never descended into: vendored dependencies and build output.
/// `node_modules` alone can hold tens of thousands of entries, and no lockfile
/// a scan should trust lives inside either. Dotfile/VCS state (`.git`,
/// `.github`, …) is pruned by the hidden-name rule below.
const PRUNED_DIRS: [&str; 2] = ["node_modules", "target"];

/// Default cap on directories one discovery pass may visit. The symlink policy
/// above already keeps the walk inside `root` (and makes cycles impossible),
/// so this only fires on a pathologically wide workspace — and then the scan
/// must fail loudly rather than silently return a partial inventory.
const DEFAULT_MAX_WALK_DIRS: usize = 10_000;

/// Plain `tokio::fs`-backed lockfile discovery.
pub struct FsLockfileDiscovery {
    max_walk_dirs: usize,
}

impl Default for FsLockfileDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

impl FsLockfileDiscovery {
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_walk_dirs: DEFAULT_MAX_WALK_DIRS,
        }
    }

    /// A tightened directory-visit bound for callers scanning a constrained
    /// workspace (tests use this to exercise the bound without staging ten
    /// thousand real directories). The root counts as one visit, so a bound
    /// of `n` admits at most `n - 1` subdirectories.
    #[must_use]
    pub fn with_walk_bound(max_walk_dirs: usize) -> Self {
        Self { max_walk_dirs }
    }
}

#[async_trait]
impl DependencyDiscoveryPort for FsLockfileDiscovery {
    async fn discover_lockfiles(&self, root: &Path) -> Result<Vec<Lockfile>, PortError> {
        let mut out = Vec::new();
        // Iterative walk: async recursion needs boxing, and a to-visit stack
        // reads better anyway.
        let mut stack = vec![root.to_path_buf()];
        let mut visited = 0usize;
        while let Some(dir) = stack.pop() {
            visited += 1;
            if visited > self.max_walk_dirs {
                return Err(PortError::Backend(format!(
                    "lockfile walk aborted after visiting {} directories under {}: \
                     the workspace exceeds the discovery bound",
                    self.max_walk_dirs,
                    root.display()
                )));
            }
            let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
                continue; // missing/unreadable subtree: nothing to scan there
            };
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                // Symlink-aware, follow-free classification (CXA-B118):
                // `DirEntry::file_type` reports the entry itself, never its
                // target — unlike `Path::is_dir`, which follows the link.
                let Ok(file_type) = entry.file_type().await else {
                    continue; // unclassifiable entry: nothing scannable there
                };
                if file_type.is_dir() {
                    let descend = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !n.starts_with('.') && !PRUNED_DIRS.contains(&n));
                    if descend {
                        stack.push(path);
                    }
                } else if file_type.is_file()
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(deps_scan::is_lockfile)
                {
                    let body = tokio::fs::read_to_string(&path)
                        .await
                        .map_err(|e| PortError::Backend(format!("read {}: {e}", path.display())))?;
                    out.push(Lockfile {
                        path: display_path(root, &path),
                        body,
                    });
                }
                // Symlinks (and every other non-regular entry) are skipped:
                // descending or reading through one would leave `root`, and a
                // scan must never see past the workspace it was given.
            }
        }
        // Deterministic order regardless of directory-read order, so reports
        // and ticket bodies are stable across runs.
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }
}

/// Repo-relative path as tickets and reports should name it; falls back to the
/// full path when `root` is not a prefix (a defensive case the walk never
/// produces).
fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    async fn write(path: PathBuf, body: &str) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.expect("mkdir");
        }
        tokio::fs::write(path, body).await.expect("write");
    }

    #[tokio::test]
    async fn finds_lockfiles_in_root_and_subdirectories_repo_relatively() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path().join("Cargo.lock"), "root").await;
        write(dir.path().join("e2e/package-lock.json"), "nested").await;
        write(dir.path().join("services/api/poetry.lock"), "deep").await;

        let found = FsLockfileDiscovery::new()
            .discover_lockfiles(dir.path())
            .await
            .expect("discovery");

        let paths: Vec<&str> = found.iter().map(|l| l.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "Cargo.lock",
                "e2e/package-lock.json",
                "services/api/poetry.lock"
            ],
            "sorted, repo-relative"
        );
        assert_eq!(found[0].body, "root");
    }

    #[tokio::test]
    async fn prunes_vendored_build_and_hidden_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path().join("Cargo.lock"), "root").await;
        // Vendored / build / VCS / dotfile state: none of these may surface.
        write(
            dir.path().join("node_modules/left-pad/package-lock.json"),
            "x",
        )
        .await;
        write(dir.path().join("target/debug/Cargo.lock"), "x").await;
        write(dir.path().join(".git/package-lock.json"), "x").await;
        write(dir.path().join(".hidden/Cargo.lock"), "x").await;

        let found = FsLockfileDiscovery::new()
            .discover_lockfiles(dir.path())
            .await
            .expect("discovery");

        assert_eq!(
            found.iter().map(|l| l.path.as_str()).collect::<Vec<_>>(),
            vec!["Cargo.lock"],
            "only the real lockfile survives"
        );
    }

    #[tokio::test]
    async fn a_missing_root_reads_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope");
        let found = FsLockfileDiscovery::new()
            .discover_lockfiles(&missing)
            .await
            .expect("empty, not an error");
        assert!(found.is_empty());
    }

    /// CXA-B118 regression: the walk escaped the workspace through symlinked
    /// directories (`Path::is_dir` follows them), e.g.
    /// `desktop/build/dmg/Applications -> /Applications` burned a core for
    /// minutes walking 332k files, and a symlink back to the root never
    /// terminated at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_directory_is_never_descended_into() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        write(dir.path().join("Cargo.lock"), "root").await;
        write(outside.path().join("package-lock.json"), "outside").await;
        // The escape (symlinked dir out of the workspace) and the cycle
        // (symlink back inside it) — the latter hung the old walk forever.
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).expect("symlink");
        std::os::unix::fs::symlink(dir.path(), dir.path().join("self")).expect("symlink");

        let found = FsLockfileDiscovery::new()
            .discover_lockfiles(dir.path())
            .await
            .expect("discovery terminates and stays inside the root");

        assert_eq!(
            found.iter().map(|l| l.path.as_str()).collect::<Vec<_>>(),
            vec!["Cargo.lock"],
            "symlinked directories are skipped, not followed"
        );
    }

    /// CXA-B118: the scan must never read PAST the root either — a symlink
    /// named like a lockfile pointing outside is not a lockfile under `root`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_lockfile_symlink_is_never_read_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let real = outside.path().join("real.lock");
        write(real.clone(), "secrets-from-outside").await;
        std::os::unix::fs::symlink(real, dir.path().join("Cargo.lock")).expect("symlink");

        let found = FsLockfileDiscovery::new()
            .discover_lockfiles(dir.path())
            .await
            .expect("discovery");

        assert!(found.is_empty(), "read through a symlink: {found:?}");
    }

    /// CXA-B118: a workspace wider than the bound fails the scan loudly —
    /// never a silently truncated inventory reported as complete.
    #[tokio::test]
    async fn a_walk_over_the_bound_errors_instead_of_truncating() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path().join("Cargo.lock"), "root").await;
        for name in ["a", "b", "c"] {
            write(dir.path().join(name).join("package-lock.json"), name).await;
        }

        let err = FsLockfileDiscovery::with_walk_bound(2)
            .discover_lockfiles(dir.path())
            .await
            .expect_err("bound exceeded");

        assert!(
            err.to_string().contains("discovery bound"),
            "the error names the bound: {err}"
        );
    }
}
