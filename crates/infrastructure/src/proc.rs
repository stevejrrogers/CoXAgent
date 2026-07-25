//! Process resource hygiene: keep agent-spawned work from starving the
//! user's machine.
//!
//! Two tools, both host-wide (shared by every project a hub runs):
//! - [`low_priority`] — spawn children under `nice` so builds/tests/agents
//!   yield CPU to interactive work instead of freezing the machine.
//! - [`heavy_gate`] — a global semaphore capping how many heavy operations
//!   (test suites, compose builds) run at once across ALL projects. One
//!   project's `cargo test` at full blast is fine; N projects at once is a
//!   denial of service on the host.

use std::ffi::OsStr;
use std::sync::LazyLock;
use tokio::process::Command;
use tokio::sync::Semaphore;

/// Default cap on concurrent heavy operations across all projects.
const DEFAULT_HEAVY_PERMITS: usize = 2;

static HEAVY_GATE: LazyLock<Semaphore> = LazyLock::new(|| {
    let permits = std::env::var("COXAGENT_MAX_PARALLEL_HEAVY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(DEFAULT_HEAVY_PERMITS);
    Semaphore::new(permits)
});

/// Acquire a slot in the host-wide heavy-operation gate. Hold the returned
/// permit for the duration of the heavy work (test run, compose build).
/// Override the cap with `COXAGENT_MAX_PARALLEL_HEAVY` (min 1, default 2).
pub async fn heavy_slot() -> tokio::sync::SemaphorePermit<'static> {
    // The gate is never closed, so acquire only fails on close — unreachable.
    HEAVY_GATE
        .acquire()
        .await
        .unwrap_or_else(|_| unreachable!("heavy gate is never closed"))
}

/// A command that runs `program` at reduced CPU priority (Unix `nice +10`),
/// so agent children — LLM CLIs and the build/test trees they spawn — stay
/// background work and the user's own machine stays responsive. On
/// non-Unix platforms it is a plain command.
pub fn low_priority(program: impl AsRef<OsStr>) -> Command {
    #[cfg(unix)]
    {
        let mut cmd = Command::new("nice");
        cmd.arg("-n").arg("10").arg(program.as_ref());
        cmd
    }
    #[cfg(not(unix))]
    {
        Command::new(program)
    }
}

/// Directories an agent CLI legitimately writes: the project workspace, temp,
/// engine/tool state and caches, and the CoXAgent workspace (logs, registry).
/// Everything else on the machine is read-only to a sandboxed agent.
fn sandbox_writable(work_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let mut allowed = vec![
        work_dir.to_path_buf(),
        std::path::PathBuf::from("/private/tmp"),
        std::path::PathBuf::from("/private/var/folders"),
        std::path::PathBuf::from("/dev"),
        home.join("CoXAgent"),
        // Engine state (sessions, auth refresh, telemetry).
        home.join(".claude"),
        home.join(".claude.json"),
        home.join(".config"),
        home.join(".cache"),
        home.join(".local"),
        // Toolchain caches the agent's builds/tests need.
        home.join(".cargo"),
        home.join(".rustup"),
        home.join(".npm"),
        home.join("go"),
        home.join(".gradle"),
        home.join(".m2"),
    ];
    if let Some(t) = std::env::var_os("TMPDIR") {
        allowed.push(std::path::PathBuf::from(t));
    }
    allowed
}

/// A macOS Seatbelt profile: allow everything EXCEPT file writes outside the
/// allow-list. Pure so it is unit-testable.
fn seatbelt_profile(writable: &[std::path::PathBuf]) -> String {
    let subpaths: String = writable
        .iter()
        .filter_map(|p| p.to_str())
        .map(|p| format!("(subpath \"{}\")", p.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ");
    format!("(version 1)(allow default)(deny file-write* (require-not (require-any {subpaths})))")
}

/// A command that runs `program` low-priority AND (when `sandbox` is on and
/// the platform supports it — macOS Seatbelt today) with file WRITES confined
/// to the project workspace + tool caches. Reads stay open (the CLIs need
/// their auth/config); the point is that a confused or hostile agent cannot
/// damage files outside its project. Platforms without a sandbox fall back to
/// plain low-priority spawning.
pub fn agent_command(
    program: impl AsRef<OsStr>,
    work_dir: &std::path::Path,
    sandbox: bool,
) -> Command {
    #[cfg(target_os = "macos")]
    {
        if sandbox {
            let profile = seatbelt_profile(&sandbox_writable(work_dir));
            let mut cmd = Command::new("sandbox-exec");
            cmd.arg("-p")
                .arg(profile)
                .arg("nice")
                .arg("-n")
                .arg("10")
                .arg(program.as_ref());
            return cmd;
        }
    }
    let _ = (work_dir, sandbox);
    low_priority(program)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn heavy_gate_caps_concurrency() {
        let a = heavy_slot().await;
        let _b = heavy_slot().await;
        // Third slot must NOT be immediately available at the default cap.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), heavy_slot())
                .await
                .is_err(),
            "third heavy slot should block at default cap of 2"
        );
        drop(a);
        // Freed slot becomes available again.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), heavy_slot())
                .await
                .is_ok()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn low_priority_actually_runs_the_program() {
        let out = low_priority("echo").arg("hi").output().await.unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }

    #[test]
    fn seatbelt_profile_denies_outside_allowlist() {
        let p = seatbelt_profile(&[std::path::PathBuf::from("/srv/proj")]);
        assert!(p.contains("(deny file-write*"));
        assert!(p.contains("(subpath \"/srv/proj\")"));
        assert!(p.starts_with("(version 1)(allow default)"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn agent_command_uses_seatbelt_when_sandboxed() {
        let c = agent_command("echo", std::path::Path::new("/srv/p"), true);
        assert_eq!(c.as_std().get_program(), "sandbox-exec");
        let c = agent_command("echo", std::path::Path::new("/srv/p"), false);
        assert_eq!(c.as_std().get_program(), "nice");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sandboxed_command_blocks_writes_outside_workspace() {
        let ws = std::env::temp_dir().join(format!("cox-sbx-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let outside = std::env::var("HOME").unwrap() + "/cox-sbx-should-never-exist";
        let script = format!("echo ok > {}/in.txt; echo x > {outside}", ws.display());
        let mut c = agent_command("/bin/sh", &ws, true);
        let out = c.arg("-c").arg(&script).output().await.unwrap();
        drop(out);
        assert!(ws.join("in.txt").exists(), "workspace write allowed");
        assert!(
            !std::path::Path::new(&outside).exists(),
            "outside write must be denied"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}
