//! Clean up orphaned test-driver processes leaked by DEV agents.
//!
//! When a project's build is broken, the DEV agent's `run_tests()` guard
//! fails after the process has already spawned subtasks (Rust / Go / Python
//! test runners). Kill them so they don't pile up burning CPU forever.
//!
//! SCOPED to the project's working directory: only processes whose command
//! line references this workspace are touched. Anything else on the machine
//! (the user's own `cargo test`, other tools, other projects) is never ours
//! to kill.

use std::path::Path;
use std::process::Command;

static ORPHAN_PATTERNS: &[&str] = &["tl_driver", "cargo test", "pytest", "go test", "npm test"];

pub fn kill_orphaned_drivers(work_dir: &Path) {
    let Some(scope) = work_dir.to_str().filter(|s| !s.trim().is_empty()) else {
        return; // no scope, no kills — never fall back to machine-wide
    };
    for pattern in ORPHAN_PATTERNS {
        let _ = kill_by_pattern(pattern, scope);
    }
}

/// Kill processes matching `pattern` whose full command line ALSO mentions
/// `scope` (the project workspace path). `pgrep -fl` prints `pid cmdline` per
/// line; the scope filter is what keeps this from ever reaping a human's own
/// test run elsewhere on the machine.
fn kill_by_pattern(pattern: &str, scope: &str) -> Result<(), std::io::Error> {
    let output = Command::new("pgrep").arg("-fl").arg(pattern).output()?;
    if !output.status.success() {
        return Ok(());
    }
    for pid in select_pids(&String::from_utf8_lossy(&output.stdout), scope) {
        let _ = Command::new("kill").arg(pid).output();
    }
    Ok(())
}

/// Pure filter: from `pgrep -fl` output (`<pid> <cmdline>` per line), the pids
/// whose command line contains `scope`.
fn select_pids(pgrep_output: &str, scope: &str) -> Vec<String> {
    pgrep_output
        .lines()
        .filter_map(|line| {
            let (pid, cmdline) = line.trim().split_once(' ')?;
            (cmdline.contains(scope) && !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()))
                .then(|| pid.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::select_pids;

    #[test]
    fn only_kills_processes_inside_the_workspace() {
        let out = "\
101 cargo test --workspace\n\
202 cargo test --manifest-path /srv/proj/codebase/Cargo.toml\n\
303 /usr/bin/pytest /srv/proj/codebase/tests\n\
404 pytest /home/user/other/tests\n";
        assert_eq!(select_pids(out, "/srv/proj/codebase"), vec!["202", "303"]);
    }

    #[test]
    fn ignores_malformed_lines() {
        assert!(select_pids("garbage\n\n", "/srv/p").is_empty());
        assert!(select_pids("abc cargo test /srv/p", "/srv/p").is_empty());
    }
}
