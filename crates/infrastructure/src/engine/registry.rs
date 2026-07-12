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
