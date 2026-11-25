// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Brownfield import admission for `POST /api/projects`: which filesystem
//! paths may be adopted as a project's codebase. Both guards here refuse
//! BEFORE the factory touches the filesystem, so a refused onboarding can
//! never scaffold a half-adopted workspace (CXA-B136's lesson).

use super::*;
use std::path::{Path, PathBuf};

/// Validate brownfield import path: refuse paths pointing at system
/// directories (`/etc`, `/proc`, …). Everything else — including a path that
/// does not exist — passes; the factory owns the remaining refusals ("codebase
/// path does not exist", per-target checks), so this guard stays the single
/// narrow system-dir veto it has always been.
pub(super) fn refused_import_path(existing: Option<&String>) -> Option<axum::response::Response> {
    let existing = existing?;
    if existing.trim().is_empty() {
        return None;
    }
    let p = Path::new(existing.trim());
    // Resolve to absolute canonical path to prevent symlink tricks.
    let real = p.canonicalize().ok()?;
    // Allow under /tmp or under $HOME (typical user repos).
    // Block system directories.
    let path_str = real.to_string_lossy();
    // Block if path equals a blocked directory, or if it starts with
    // a blocked directory plus '/', to catch `/private/etc/foo` etc.
    let blocked_prefixes = [
        "/etc",
        "/private/etc",
        "/root",
        "/var/run",
        "/var/log",
        "/usr/lib",
        "/usr/sbin",
        "/bin",
        "/sbin",
        "/dev",
        "/proc",
        "/sys",
    ];
    let blocked = blocked_prefixes.iter().any(|pfx| {
        path_str == *pfx
            || path_str.starts_with(pfx)
                && path_str.as_bytes().get(pfx.len()).copied() == Some(b'/')
    });
    blocked.then(|| {
        (
            StatusCode::FORBIDDEN,
            "cannot import from this path".to_owned(),
        )
            .into_response()
    })
}

/// CXA-B145: a brownfield import adopts the codebase IN PLACE, and nothing
/// stopped the path from being the working tree of a project that is
/// registered and live right now — two runners then do branch-per-ticket git,
/// deploys and version bumps over one tree and silently corrupt each other.
/// Refuse the adoption with 409 when the requested path overlaps the codebase
/// of any registered project (the same tree, a subtree of it, or a parent of
/// it), before the factory runs.
pub(super) async fn refused_import_owned_by_registered(
    app: &AppState,
    existing: Option<&String>,
) -> Option<axum::response::Response> {
    let raw = existing
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    // Canonicalise both sides, so a symlink alias into another project's
    // workspace, `..` segments or a /tmp → /private/tmp difference compare as
    // the directories they really are. A path that does not exist fails the
    // canonicalise and falls through: the factory refuses it with its honest
    // "codebase path does not exist" instead.
    let requested = Path::new(raw).canonicalize().ok()?;
    let registered = app.projects.read().await;
    let owned: Vec<(String, PathBuf)> = registered
        .values()
        .filter_map(|p| Some((p.id.clone(), p.work_dir.canonicalize().ok()?)))
        .collect();
    drop(registered);
    codebase_owned_by(&requested, &owned).map(|id| {
        conflict_error(&format!(
            "codebase at {raw} is already the working tree of project '{id}' — refusing to \
             adopt one codebase into two projects"
        ))
    })
}

/// Pure decision over canonical paths: the id of the registered project whose
/// codebase overlaps `requested`, if any. Containment counts in EITHER
/// direction — adopting a parent of another project's tree drags its whole
/// workspace along, adopting a subtree of it shares its git operations.
fn codebase_owned_by(requested: &Path, registered: &[(String, PathBuf)]) -> Option<String> {
    registered
        .iter()
        .find(|(_, owned)| requested.starts_with(owned) || owned.starts_with(requested))
        .map(|(id, _)| id.clone())
}

#[cfg(test)]
mod codebase_owned_by_tests {
    use super::codebase_owned_by;
    use std::path::{Path, PathBuf};

    fn registry(entries: &[(&str, &str)]) -> Vec<(String, PathBuf)> {
        entries
            .iter()
            .map(|(id, p)| ((*id).to_owned(), PathBuf::from(*p)))
            .collect()
    }

    #[test]
    fn the_exact_tree_of_a_registered_project_is_owned() {
        let owned = registry(&[("default", "/ws/default/codebase")]);
        assert_eq!(
            codebase_owned_by(Path::new("/ws/default/codebase"), &owned),
            Some("default".to_owned())
        );
    }

    #[test]
    fn a_subtree_of_a_registered_codebase_is_owned() {
        let owned = registry(&[("default", "/ws/default/codebase")]);
        assert_eq!(
            codebase_owned_by(Path::new("/ws/default/codebase/crates/app"), &owned),
            Some("default".to_owned())
        );
    }

    #[test]
    fn a_parent_of_a_registered_codebase_is_owned() {
        // Adopting the project workspace itself drags its whole codebase (and
        // its state dir) into the new project's tree — same corruption.
        let owned = registry(&[("default", "/ws/default/codebase")]);
        assert_eq!(
            codebase_owned_by(Path::new("/ws/default"), &owned),
            Some("default".to_owned())
        );
    }

    #[test]
    fn a_sibling_path_is_free() {
        let owned = registry(&[("default", "/ws/default/codebase")]);
        assert_eq!(
            codebase_owned_by(Path::new("/ws/other/codebase"), &owned),
            None
        );
    }

    #[test]
    fn a_path_merely_sharing_a_string_prefix_is_free() {
        // Comparison is component-wise (`Path::starts_with`): the archive is
        // NOT the default tree, however much the strings overlap.
        let owned = registry(&[("default", "/ws/default/codebase")]);
        assert_eq!(
            codebase_owned_by(Path::new("/ws/default/codebase-archive"), &owned),
            None
        );
    }
}
