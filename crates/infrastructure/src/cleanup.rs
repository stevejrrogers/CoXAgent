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
//!
//! AGE-GATED: only processes running longer than [`MIN_ORPHAN_AGE_MINUTES`]
//! are orphans. A fresh `cargo test` matching the workspace is a SIBLING
//! runner's legitimate in-flight DoD check — killing it made that runner's
//! verify fail spuriously (the "cleanup kills the server" incident). A real
//! leak has, by definition, been alive for a long time.

use std::path::Path;
use std::process::Command;

/// Port adapter: the OS process janitor.
pub struct OsProcessJanitor;

impl coxagent_application::ports::outbound::ProcessJanitorPort for OsProcessJanitor {
    fn kill_orphaned_drivers(&self, work_dir: &Path) {
        kill_orphaned_drivers(work_dir);
    }
}

static ORPHAN_PATTERNS: &[&str] = &["tl_driver", "cargo test", "pytest", "go test", "npm test"];

pub fn kill_orphaned_drivers(work_dir: &Path) {
    let Some(scope) = work_dir.to_str().filter(|s| !s.trim().is_empty()) else {
        return; // no scope, no kills — never fall back to machine-wide
    };
    let my_pid = std::process::id().to_string();
    for pattern in ORPHAN_PATTERNS {
        let _ = kill_by_pattern(pattern, scope, &my_pid);
    }
}

/// Kill processes matching `pattern` whose full command line ALSO mentions
/// `scope` (the project workspace path). `pgrep -fl` prints `pid cmdline` per
/// line; the scope filter is what keeps this from ever reaping a human's own
/// test run elsewhere on the machine.
fn kill_by_pattern(pattern: &str, scope: &str, my_pid: &str) -> Result<(), std::io::Error> {
    let output = Command::new("pgrep").arg("-fl").arg(pattern).output()?;
    if !output.status.success() {
        return Ok(());
    }
    for pid in select_pids(&String::from_utf8_lossy(&output.stdout), scope) {
        if pid == my_pid {
            continue;
        }
        if let Ok(pid_num) = pid.parse::<i32>() {
            if pid_num < 100 {
                continue; // never kill system processes
            }
            // Only long-lived processes are orphans; fresh ones are a sibling
            // runner's active work.
            if !is_old_enough(pid_num) {
                continue;
            }
            let _ = Command::new("kill").arg(pid).output();
        }
    }
    Ok(())
}

/// Minimum age before a matching process counts as leaked.
const MIN_ORPHAN_AGE_MINUTES: u64 = 15;

/// Whether `pid` has been alive longer than [`MIN_ORPHAN_AGE_MINUTES`],
/// via `ps -o etime=`. Unknown/parse-failure = NOT old enough (never kill
/// on uncertainty).
fn is_old_enough(pid: i32) -> bool {
    let Ok(out) = Command::new("ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    etime_minutes(String::from_utf8_lossy(&out.stdout).trim()) >= MIN_ORPHAN_AGE_MINUTES
}

/// Parse `ps` etime (`MM:SS`, `HH:MM:SS`, or `D-HH:MM:SS`) into whole
/// minutes. Unparseable input = 0 (treated as young).
fn etime_minutes(etime: &str) -> u64 {
    let (days, clock) = match etime.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().unwrap_or(0), rest),
        None => (0, etime),
    };
    let parts: Vec<u64> = clock
        .split(':')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0))
        .collect();
    let (h, m) = match parts.as_slice() {
        [_ss] => (0, 0),
        [mm, _ss] => (0, *mm),
        [hh, mm, _ss] => (*hh, *mm),
        _ => (0, 0),
    };
    days * 24 * 60 + h * 60 + m
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
    fn etime_parses_all_ps_formats() {
        use super::etime_minutes;
        assert_eq!(etime_minutes("45"), 0); // 45 s — young
        assert_eq!(etime_minutes("05:12"), 5);
        assert_eq!(etime_minutes("01:02:03"), 62);
        assert_eq!(etime_minutes("2-01:00:00"), 2 * 24 * 60 + 60);
        assert_eq!(etime_minutes("garbage"), 0, "unparseable = young = spared");
    }

    #[test]
    fn fresh_process_is_not_old_enough() {
        // This test process itself is seconds old.
        assert!(!super::is_old_enough(
            std::process::id().try_into().unwrap()
        ));
    }

    #[test]
    fn ignores_malformed_lines() {
        assert!(select_pids("garbage\n\n", "/srv/p").is_empty());
        assert!(select_pids("abc cargo test /srv/p", "/srv/p").is_empty());
    }
}
