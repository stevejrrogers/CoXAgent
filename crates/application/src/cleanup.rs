//! Clean up orphaned test-driver processes leaked by DEV agents.
//!
//! When a project's build is broken, the DEV agent's `run_tests()` guard
//! fails after the process has already spawned subtasks (Rust / Go / Python
//! test runners). Kill them so they don't pile up burning CPU forever.

use std::process::Command;

static ORPHAN_PATTERNS: &[&str] = &[
    "tl_driver",
    "cargo test",
    "pytest",
    "go test",
    "npm test",
];

pub async fn kill_orphaned_drivers() {
    for pattern in ORPHAN_PATTERNS {
        let _ = kill_by_pattern(pattern);
    }
}

fn kill_by_pattern(pattern: &str) -> Result<(), std::io::Error> {
    let output = Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .output()?;
    if !output.status.success() {
        return Ok(());
    }
    let pids = String::from_utf8_lossy(&output.stdout);
    for pid in pids.lines() {
        let pid = pid.trim();
        if pid.is_empty() {
            continue;
        }
        if let Ok(pid_num) = pid.parse::<i32>() {
            let _ = Command::new("kill").arg(pid_num.to_string()).output();
        }
    }
    Ok(())
}
