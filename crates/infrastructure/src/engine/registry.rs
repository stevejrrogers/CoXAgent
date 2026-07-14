//! Engine discovery — scan `PATH` for known agent CLIs so the app can report a
//! machine's capabilities and the settings UI can map roles to installed engines.

use coxagent_application::config::EngineKind;
use std::path::{Path, PathBuf};

/// A discovered engine binary on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedEngine {
    pub kind: EngineKind,
    pub path: PathBuf,
}

/// Discover known engines on the current process `PATH`.
#[must_use]
pub fn discover() -> Vec<DetectedEngine> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    discover_in(&dirs)
}

/// Discover known engines by scanning the given directories. Pure over its
/// inputs, so tests can point it at a temp dir with fake executables.
#[must_use]
pub fn discover_in(dirs: &[PathBuf]) -> Vec<DetectedEngine> {
    let mut found = Vec::new();
    for &kind in EngineKind::all() {
        if let Some(path) = find_binary(dirs, kind.as_binary()) {
            found.push(DetectedEngine { kind, path });
        }
    }
    found
}

/// A developer tool the git/deploy flow may need, and whether it is installed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DetectedTool {
    /// Binary name (`git`, `gh`, `glab`, `docker`).
    pub name: &'static str,
    /// What CoXAgent uses it for.
    pub purpose: &'static str,
    /// Whether it was found on `PATH`.
    pub present: bool,
    /// Full path when present.
    pub path: String,
    /// Shell command to install it (macOS/Homebrew).
    pub install: &'static str,
    /// Whether it needs an interactive auth step only the user can do.
    pub needs_auth: bool,
}

/// The tools the git flow and deploy step rely on.
const TOOLING: &[(&str, &str, &str, bool)] = &[
    (
        "git",
        "version control (branch/commit)",
        "brew install git",
        false,
    ),
    (
        "gh",
        "GitHub PRs & auth",
        "brew install gh && gh auth login",
        true,
    ),
    (
        "glab",
        "GitLab MRs & auth",
        "brew install glab && glab auth login",
        true,
    ),
    (
        "docker",
        "deploy (docker compose)",
        "brew install --cask docker",
        false,
    ),
];

/// Detect the developer tooling on `PATH`, so the dashboard can show what's
/// missing and how to install it.
#[must_use]
pub fn discover_tooling() -> Vec<DetectedTool> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    TOOLING
        .iter()
        .map(|&(name, purpose, install, needs_auth)| {
            let found = find_binary(&dirs, name);
            DetectedTool {
                name,
                purpose,
                present: found.is_some(),
                path: found.map(|p| p.display().to_string()).unwrap_or_default(),
                install,
                needs_auth,
            }
        })
        .collect()
}

/// Return the first directory containing an executable named `name`.
fn find_binary(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            Some(candidate)
        } else {
            None
        }
    })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}
