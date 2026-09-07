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

    fn purge_target_cache(&self, work_dir: &Path) {
        purge_target_cache(work_dir);
    }
}

/// Best-effort removal of a build cache directory. Scoped to exactly
/// `work_dir/target`, never touches anything else, and is never fatal —
/// a cache we cannot remove just costs the next rebuild, not correctness.
pub fn purge_target_cache(work_dir: &Path) {
    let target = work_dir.join("target");
    if !target.exists() {
        return; // nothing cached — nothing to do
    }
    let _ = std::fs::remove_dir_all(&target);
}

static ORPHAN_PATTERNS: &[&str] = &["tl_driver", "cargo test", "pytest", "go test", "npm test"];

/// Agent engine CLIs whose command line names the workspace (`--dir`/
/// `--add-dir`). A running hub restart does NOT take its in-flight engine
/// children with it — they reparent to PID 1 and keep editing the codebase
/// while the new hub claims the same tickets, so two agents trample one
/// working tree. Reparenting IS the proof of orphanhood, so these are killed
/// on `ppid == 1` alone (no age gate): a sibling operator's live engine still
/// has its living parent and is never touched.
static ENGINE_PATTERNS: &[&str] = &["opencode", "copilot"];

pub fn kill_orphaned_drivers(work_dir: &Path) {
    let Some(scope) = work_dir.to_str().filter(|s| !s.trim().is_empty()) else {
        return; // no scope, no kills — never fall back to machine-wide
    };
    let my_pid = std::process::id().to_string();
    for pattern in ORPHAN_PATTERNS {
        let _ = kill_by_pattern(pattern, scope, &my_pid);
    }
    for pattern in ENGINE_PATTERNS {
        let _ = kill_reparented_engines(pattern, scope, &my_pid);
        let _ = kill_overbudget_engines(pattern, scope, &my_pid);
    }
}

/// Wall-clock budget for a single engine run (CXA-F346). A legitimate run
/// finishes in minutes; the operator-observed zombies (engine wedged on a
/// dead network read while the hub-side caller had already failed over and
/// DROPPED the future — so the adapter's own timeout-kill branch never ran)
/// sat for 45 minutes to 3 DAYS. The budget sits well above the adapters'
/// request timeouts and above [`MIN_ORPHAN_AGE_MINUTES`], so anything past it
/// is a leak by definition, hub alive or not.
const ENGINE_BUDGET_MINUTES: u64 = 50;

/// Kill engine processes matching `pattern` whose command line mentions the
/// workspace AND that have outlived [`ENGINE_BUDGET_MINUTES`] — regardless of
/// parentage. This is the second reaper net: `kill_reparented_engines` catches
/// children whose hub died; this catches children whose hub is alive but
/// whose awaiting future was dropped on failover, leaving the child wedged
/// forever. Never by name alone: workspace scope + age budget both gate.
/// TERM first (the CLI flushes transcripts on TERM), then KILL the whole
/// process group — each engine spawn is its own group leader (proc.rs), so
/// `kill -9 -<pid>` reaps grandchildren too.
fn kill_overbudget_engines(pattern: &str, scope: &str, my_pid: &str) -> Result<(), std::io::Error> {
    let output = Command::new("pgrep").arg("-fl").arg(pattern).output()?;
    if !output.status.success() {
        return Ok(());
    }
    for pid in select_pids(&String::from_utf8_lossy(&output.stdout), scope) {
        if pid == my_pid {
            continue;
        }
        let Ok(pid_num) = pid.parse::<i32>() else {
            continue;
        };
        if pid_num < 100 || !engine_over_budget(pid_num) {
            continue;
        }
        let _ = Command::new("kill").args(["-TERM", &pid]).output();
        std::thread::sleep(std::time::Duration::from_secs(2));
        // Escalate to the process GROUP so a wedged child's own children die
        // with it. Harmless if TERM already worked (kill of a gone group is
        // an ignored error).
        let _ = Command::new("kill")
            .args(["-9", &format!("-{pid}")])
            .output();
    }
    Ok(())
}

/// Whether `pid` has outlived [`ENGINE_BUDGET_MINUTES`]. Unknown = within
/// budget (never kill on uncertainty).
fn engine_over_budget(pid: i32) -> bool {
    let Ok(out) = Command::new("ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    etime_minutes(String::from_utf8_lossy(&out.stdout).trim()) >= ENGINE_BUDGET_MINUTES
}

/// Kill engine processes matching `pattern` whose command line mentions the
/// workspace AND whose parent is PID 1 — i.e. their spawning runner is gone.
fn kill_reparented_engines(pattern: &str, scope: &str, my_pid: &str) -> Result<(), std::io::Error> {
    let output = Command::new("pgrep").arg("-fl").arg(pattern).output()?;
    if !output.status.success() {
        return Ok(());
    }
    for pid in select_pids(&String::from_utf8_lossy(&output.stdout), scope) {
        if pid == my_pid {
            continue;
        }
        let Ok(pid_num) = pid.parse::<i32>() else {
            continue;
        };
        if pid_num < 100 || !is_reparented(pid_num) {
            continue;
        }
        let _ = Command::new("kill").arg(&pid).output();
    }
    Ok(())
}

/// Whether `pid`'s parent is PID 1 (launchd/init) — the spawner died. Unknown
/// = NOT reparented (never kill on uncertainty).
fn is_reparented(pid: i32) -> bool {
    let Ok(out) = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout).trim() == "1"
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
// 45, deliberately ABOVE the 30-minute DoD/boot-check window: the suite
// legitimately runs 12-25 minutes cold, and at 15 the janitor shot every
// sibling runner's in-flight boot check — truncated output, stray "kill:"
// lines, and a week of DEV looking dead behind quiet cycle errors.
const MIN_ORPHAN_AGE_MINUTES: u64 = 45;

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
    use super::{purge_target_cache, select_pids};

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
    fn fresh_engine_is_within_budget() {
        // This test process itself is seconds old — never over the 50-minute
        // engine budget, so the second reaper net must spare it.
        assert!(!super::engine_over_budget(
            std::process::id().try_into().unwrap()
        ));
    }

    // The budget must stay ABOVE the generic 45-minute orphan age: the engine
    // net is the LAST resort, never the first to fire. Compile-time law.
    const _BUDGET_ABOVE_ORPHAN_GATE: () =
        assert!(super::ENGINE_BUDGET_MINUTES >= super::MIN_ORPHAN_AGE_MINUTES);

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

    #[test]
    fn purge_target_removes_only_the_build_cache() {
        let tmp = std::env::temp_dir().join(format!("cxa-purge-test-{}", std::process::id()));
        let target = tmp.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("artifacts.bin"), b"cache").unwrap();
        std::fs::write(tmp.join("source.rs"), b"let code = 1;").unwrap();

        purge_target_cache(&tmp);

        assert!(!target.exists(), "target must be purged");
        assert!(
            tmp.join("source.rs").exists(),
            "non-cache source files must survive"
        );
        // Second call on an already-clean dir is a no-op, never a panic.
        purge_target_cache(&tmp);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn purge_target_is_a_noop_when_nothing_cached() {
        let tmp = std::env::temp_dir().join(format!("cxa-purge-null-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        purge_target_cache(&tmp); // no `target` subdir — must not error
        assert!(tmp.exists());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
