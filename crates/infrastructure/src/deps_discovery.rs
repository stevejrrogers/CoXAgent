//! `FsLockfileDiscovery` — the [`DependencyDiscoveryPort`] adapter over the
//! real filesystem (CXA-B111): walks a workspace root, skips the directories
//! that can never carry a scannable lock, and reads every file the scanner's
//! own contract recognises. Until this shipped, discovery existed only as
//! test-only IO inside the F009 gate and the scanner had no production caller.

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

/// Plain `tokio::fs`-backed lockfile discovery.
#[derive(Default)]
pub struct FsLockfileDiscovery;

impl FsLockfileDiscovery {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DependencyDiscoveryPort for FsLockfileDiscovery {
    async fn discover_lockfiles(&self, root: &Path) -> Result<Vec<Lockfile>, PortError> {
        let mut out = Vec::new();
        // Iterative walk: async recursion needs boxing, and a to-visit stack
        // reads better anyway.
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
                continue; // missing/unreadable subtree: nothing to scan there
            };
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                if path.is_dir() {
                    let descend = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !n.starts_with('.') && !PRUNED_DIRS.contains(&n));
                    if descend {
                        stack.push(path);
                    }
                } else if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(deps_scan::is_lockfile)
                {
                    let body = tokio::fs::read_to_string(&path).await.map_err(|e| {
                        PortError::Backend(format!("read {}: {e}", path.display()))
                    })?;
                    out.push(Lockfile {
                        path: display_path(root, &path),
                        body,
                    });
                }
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
            vec!["Cargo.lock", "e2e/package-lock.json", "services/api/poetry.lock"],
            "sorted, repo-relative"
        );
        assert_eq!(found[0].body, "root");
    }

    #[tokio::test]
    async fn prunes_vendored_build_and_hidden_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path().join("Cargo.lock"), "root").await;
        // Vendored / build / VCS / dotfile state: none of these may surface.
        write(dir.path().join("node_modules/left-pad/package-lock.json"), "x").await;
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
}
