//! Discovery is hermetic: it scans directories we control, so we can assert on
//! fake executables without depending on what's installed on the CI runner.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::config::EngineKind;
use coxagent_infrastructure::engine::discover_in;

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, "#!/bin/sh\n").unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

#[cfg(unix)]
#[test]
fn finds_executables_and_ignores_non_executables() {
    let dir = tempfile::tempdir().unwrap();
    // opencode: executable -> detected.
    make_executable(&dir.path().join("opencode"));
    // claude: present but NOT executable -> ignored.
    std::fs::write(dir.path().join("claude"), "not exec").unwrap();

    let found = discover_in(&[dir.path().to_path_buf()]);
    let kinds: Vec<_> = found.iter().map(|d| d.kind).collect();

    assert!(kinds.contains(&EngineKind::Opencode));
    assert!(!kinds.contains(&EngineKind::Claude));
}

#[test]
fn empty_dirs_find_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(discover_in(&[dir.path().to_path_buf()]).is_empty());
}
