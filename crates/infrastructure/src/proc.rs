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
}
