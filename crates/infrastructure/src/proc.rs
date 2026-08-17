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
//!
//! A third concern lives here too: [`agent_command`] confines an agent CLI's
//! file WRITES to the project workspace + tool caches, so a confused or
//! hostile agent cannot damage files outside its project. macOS uses Seatbelt
//! (`sandbox-exec`); Linux uses Bubblewrap (`bwrap`) when it's on `PATH`.
//! Platforms/hosts without a supported mechanism run unconfined and report
//! [`SandboxStatus::Unavailable`] rather than silently pretending to be safe.

/// The macOS Seatbelt apply-failure policy (retry, then report the
/// confinement actually in force) — its own concern, its own file.
mod seatbelt;

pub use seatbelt::{output_confined, spawn_confined};

use coxagent_application::ports::outbound::SandboxStatus;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
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
        // Own process group: the spawned CLI leads it, every child it spawns
        // (builds, dev servers) inherits it — so a timeout can kill the WHOLE
        // tree via kill_group, not just the direct child.
        cmd.process_group(0);
        cmd
    }
    #[cfg(not(unix))]
    {
        Command::new(program)
    }
}

/// Directories every agent CLI legitimately writes, on every platform: the
/// project workspace, the CoXAgent workspace (logs, registry), engine/tool
/// state, and toolchain caches the agent's builds/tests need.
fn sandbox_writable_base(work_dir: &Path, home: &Path) -> Vec<PathBuf> {
    let mut allowed = vec![
        work_dir.to_path_buf(),
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
        allowed.push(PathBuf::from(t));
    }
    allowed
}

/// The current process's real uid, read from `/proc/self/status` (no `libc`
/// dependency needed for one number).
#[cfg(target_os = "linux")]
fn current_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| {
        l.strip_prefix("Uid:")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

/// Directories an agent CLI legitimately writes on this platform. Everything
/// else on the machine stays read-only to a sandboxed agent.
fn sandbox_writable(work_dir: &Path) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut allowed = sandbox_writable_base(work_dir, &home);
    #[cfg(target_os = "macos")]
    {
        allowed.push(PathBuf::from("/private/tmp"));
        allowed.push(PathBuf::from("/private/var/folders"));
        allowed.push(PathBuf::from("/dev"));
    }
    #[cfg(target_os = "linux")]
    {
        allowed.push(PathBuf::from("/tmp"));
        allowed.push(PathBuf::from("/dev/shm"));
        if let Some(uid) = current_uid() {
            allowed.push(PathBuf::from(format!("/run/user/{uid}")));
        }
    }
    allowed
}

/// A macOS Seatbelt profile: allow everything EXCEPT file writes outside the
/// allow-list. Pure so it is unit-testable.
///
/// The gate is load-bearing (COX-B006): the only call site is macOS-only, so
/// without it this is dead code on Linux and `warnings = "deny"` fails the
/// Docker release build. `test` keeps it available to the unit test on every
/// host. Guarded repo-wide by `crates/app/tests/platform_gates.rs`.
#[cfg(any(test, target_os = "macos"))]
fn seatbelt_profile(writable: &[PathBuf]) -> String {
    let subpaths: String = writable
        .iter()
        .filter_map(|p| p.to_str())
        .map(|p| format!("(subpath \"{}\")", p.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ");
    format!("(version 1)(allow default)(deny file-write* (require-not (require-any {subpaths})))")
}

/// Whether `bwrap` (Bubblewrap) is on `PATH`. Probed once per process — the
/// answer cannot change without a process restart, and the check spawns a
/// child, so it is not worth repeating on every run.
#[cfg(target_os = "linux")]
fn bwrap_available() -> bool {
    static AVAILABLE: LazyLock<bool> = LazyLock::new(|| {
        std::process::Command::new("bwrap")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    });
    *AVAILABLE
}

/// Bubblewrap arguments confining writes to `writable`, mirroring the Seatbelt
/// allow-write-list under a genuinely different mount model: `--ro-bind / /`
/// makes the WHOLE filesystem readable (so reads stay open everywhere, exactly
/// like Seatbelt's `(allow default)`), `--proc`/`--dev-bind` restore working
/// `/proc` and `/dev`, and each allowlist entry is bound read-write LAST so it
/// overrides the read-only root for just that subpath. Pure over its input —
/// callers are responsible for making sure `writable` paths already exist
/// (`--bind` fails hard on a missing source; that's the point: a missing cache
/// dir should be created and made writable, not silently fall through to
/// read-only).
#[cfg(any(test, target_os = "linux"))]
fn bwrap_args(writable: &[PathBuf]) -> Vec<String> {
    let mut args = vec![
        "--unshare-pid".to_owned(),
        "--die-with-parent".to_owned(),
        "--ro-bind".to_owned(),
        "/".to_owned(),
        "/".to_owned(),
        "--proc".to_owned(),
        "/proc".to_owned(),
        "--dev-bind".to_owned(),
        "/dev".to_owned(),
        "/dev".to_owned(),
    ];
    for p in writable {
        if let Some(s) = p.to_str() {
            args.push("--bind".to_owned());
            args.push(s.to_owned());
            args.push(s.to_owned());
        }
    }
    args
}

/// The write-confinement `agent_command` would apply right now, given
/// `sandbox` and the host platform — without actually building a command.
/// Cheap: the underlying `bwrap` probe is memoized per process.
#[must_use]
pub fn sandbox_status(sandbox: bool) -> SandboxStatus {
    if !sandbox {
        return SandboxStatus::NotRequested;
    }
    #[cfg(target_os = "macos")]
    {
        SandboxStatus::Confined("seatbelt")
    }
    #[cfg(target_os = "linux")]
    {
        if bwrap_available() {
            SandboxStatus::Confined("bwrap")
        } else {
            SandboxStatus::Unavailable("bwrap not found on PATH")
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        SandboxStatus::Unavailable("sandboxing not supported on this platform")
    }
}

/// Build the confined command for a host that supports it. Only called once
/// `sandbox_status` has already established confinement applies.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn confined_command(program: impl AsRef<OsStr>, work_dir: &Path) -> Command {
    #[cfg(target_os = "macos")]
    {
        let profile = seatbelt_profile(&sandbox_writable(work_dir));
        let mut cmd = Command::new("sandbox-exec");
        cmd.arg("-p")
            .arg(profile)
            .arg("nice")
            .arg("-n")
            .arg("10")
            .arg(program.as_ref());
        cmd.process_group(0);
        cmd
    }
    #[cfg(target_os = "linux")]
    {
        let writable = sandbox_writable(work_dir);
        // Pre-create allowlist dirs so a fresh host (no `~/.cargo` yet) gets a
        // writable bind, not a silent fall-through to the read-only root bind.
        for p in &writable {
            let _ = std::fs::create_dir_all(p);
        }
        let mut cmd = Command::new("bwrap");
        for a in bwrap_args(&writable) {
            cmd.arg(a);
        }
        cmd.arg("nice").arg("-n").arg("10").arg(program.as_ref());
        cmd
    }
}

/// A command that runs `program` low-priority AND (when `sandbox` is on and
/// the platform supports it — macOS Seatbelt or Linux Bubblewrap) with file
/// WRITES confined to the project workspace + tool caches. Reads stay open
/// (the CLIs need their auth/config); the point is that a confused or hostile
/// agent cannot damage files outside its project. A platform/host without a
/// supported mechanism NEVER hard-fails the run — it falls back to plain
/// low-priority spawning and reports [`SandboxStatus::Unavailable`] so the
/// caller can warn instead of silently running unconfined.
pub fn agent_command(
    program: impl AsRef<OsStr>,
    work_dir: &Path,
    sandbox: bool,
) -> (Command, SandboxStatus) {
    let status = sandbox_status(sandbox);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if matches!(status, SandboxStatus::Confined(_)) {
        return (confined_command(program, work_dir), status);
    }
    let _ = work_dir;
    (low_priority(program), status)
}

/// Kill the ENTIRE process group led by `pid` (spawned with
/// `process_group(0)`), reaping every descendant — the direct child's own
/// `kill_on_drop` only takes out the leader and orphans its children (a
/// stray `npm run dev` from a timed-out agent would otherwise live on).
/// Best-effort; no-op on non-unix.
pub fn kill_group(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-9", "--", &format!("-{pid}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    let _ = pid;
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
        let (c, status) = agent_command("echo", std::path::Path::new("/srv/p"), true);
        assert_eq!(c.as_std().get_program(), "sandbox-exec");
        assert_eq!(status, SandboxStatus::Confined("seatbelt"));
        let (c, status) = agent_command("echo", std::path::Path::new("/srv/p"), false);
        assert_eq!(c.as_std().get_program(), "nice");
        assert_eq!(status, SandboxStatus::NotRequested);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sandboxed_command_blocks_writes_outside_workspace() {
        let ws = std::env::temp_dir().join(format!("cox-sbx-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let outside = std::env::var("HOME").unwrap() + "/cox-sbx-should-never-exist";
        let script = format!("echo ok > {}/in.txt; echo x > {outside}", ws.display());
        let (mut c, requested) = agent_command("/bin/sh", &ws, true);
        let (out, effective) = output_confined(c.arg("-c").arg(&script), requested)
            .await
            .unwrap();
        // COX-B022: some hosts refuse `sandbox_apply()` for this process no
        // matter how often it is retried — measured on this repo's own CI/dev
        // boxes, where the failure arrives in bursts that outlast the whole
        // retry loop (a probe of 400 consecutive calls in a warm process saw
        // zero failures, while a process inside a failing window saw 20 of 20
        // fail). `sandbox-exec` then exits 71 before the script runs, so NOTHING
        // about the write policy was exercised and asserting on it would report
        // an OS condition as a code regression — the false-fail this ticket is
        // about. The retry policy itself is pinned deterministically by the
        // `seatbelt` module's tests, so skipping here gives up no coverage of
        // the fix. COX-B016: the skip now keys on the status the helper itself
        // reports, so the test and production code agree on what "the OS
        // refused" means instead of re-deriving it from an exit code.
        if effective == SandboxStatus::Denied("seatbelt") {
            eprintln!(
                "SKIPPED sandboxed_command_blocks_writes_outside_workspace: this host \
                 refused to apply a Seatbelt profile on every attempt ({}) — the \
                 write policy was never exercised",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            let _ = std::fs::remove_dir_all(&ws);
            return;
        }
        assert_eq!(
            effective, requested,
            "an applied profile must still report as confined"
        );
        // Any OTHER failure is ours: say which, so exit 71 is never confused
        // with the target program's own status.
        assert!(
            ws.join("in.txt").exists(),
            "workspace write allowed — got exit {:?}, stderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        assert!(
            !std::path::Path::new(&outside).exists(),
            "outside write must be denied"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // --- COX-F003: Linux workspace sandboxing (Bubblewrap) ---------------
    //
    // Mirrors the macOS Seatbelt tests above. Linux confinement uses `bwrap`
    // (Bubblewrap) the same way macOS uses `sandbox-exec`: writes outside the
    // workspace/tool-cache allowlist are denied, reads stay open everywhere
    // (agent CLIs need their auth/config files).

    #[test]
    fn bwrap_args_ro_binds_root_proc_dev_and_allowlist() {
        let args = bwrap_args(&[std::path::PathBuf::from("/srv/proj")]);
        let idx = |needle: &str| args.iter().position(|a| a == needle);
        let unshare = idx("--unshare-pid").expect("--unshare-pid present");
        let die = idx("--die-with-parent").expect("--die-with-parent present");
        let ro_bind = idx("--ro-bind").expect("--ro-bind present");
        let proc_flag = idx("--proc").expect("--proc present");
        let dev_bind = idx("--dev-bind").expect("--dev-bind present");
        let bind = idx("--bind").expect("--bind present");
        assert!(unshare < ro_bind && die < ro_bind, "namespace flags first");
        assert_eq!(args[ro_bind + 1], "/");
        assert_eq!(args[ro_bind + 2], "/", "whole filesystem read-only first");
        assert!(ro_bind < proc_flag, "--ro-bind / / before --proc override");
        assert!(proc_flag < dev_bind, "--proc before --dev-bind override");
        assert!(
            dev_bind < bind,
            "--proc/--dev-bind overrides before the allowlist"
        );
        assert_eq!(args[bind + 1], "/srv/proj");
        assert_eq!(args[bind + 2], "/srv/proj");
    }

    /// AC: with `sandbox: true` on Linux and `bwrap` present, `agent_command`
    /// must invoke the command through `bwrap`, exactly as it invokes
    /// `sandbox-exec` on macOS in `agent_command_uses_seatbelt_when_sandboxed`.
    #[cfg(target_os = "linux")]
    #[test]
    fn agent_command_uses_bwrap_when_sandboxed() {
        let (c, status) = agent_command("echo", std::path::Path::new("/srv/p"), true);
        if bwrap_available() {
            assert_eq!(
                c.as_std().get_program(),
                "bwrap",
                "Linux must confine writes via bwrap when sandboxed, mirroring \
                 macOS Seatbelt"
            );
            assert_eq!(status, SandboxStatus::Confined("bwrap"));
        } else {
            assert_eq!(c.as_std().get_program(), "nice");
            assert_eq!(
                status,
                SandboxStatus::Unavailable("bwrap not found on PATH")
            );
        }
        let (c, status) = agent_command("echo", std::path::Path::new("/srv/p"), false);
        assert_eq!(c.as_std().get_program(), "nice");
        assert_eq!(status, SandboxStatus::NotRequested);
    }

    /// AC: under `bwrap`, writes outside the workspace/tool-cache allowlist
    /// are denied while reads outside the allowlist remain unrestricted —
    /// the same contract `sandboxed_command_blocks_writes_outside_workspace`
    /// verifies for macOS Seatbelt.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn sandboxed_command_blocks_writes_outside_workspace_on_linux() {
        if !bwrap_available() {
            return; // no bwrap on this host — nothing to verify here.
        }
        let ws = std::env::temp_dir().join(format!("cox-sbx-linux-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let home = std::env::var("HOME").unwrap();
        let outside = format!("{home}/cox-sbx-should-never-exist");
        // `/etc/passwd` always exists on Linux and sits outside the
        // allowlist — reading it must still succeed under confinement.
        let script = format!(
            "cat /etc/passwd > /dev/null && echo READ_OK; echo ok > {}/in.txt; echo x > {outside}",
            ws.display()
        );
        let (mut c, status) = agent_command("/bin/sh", &ws, true);
        assert_eq!(status, SandboxStatus::Confined("bwrap"));
        let out = c.arg("-c").arg(&script).output().await.unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("READ_OK"),
            "reads outside the allowlist must remain unrestricted under bwrap, \
             exactly like Seatbelt on macOS today"
        );
        assert!(ws.join("in.txt").exists(), "workspace write allowed");
        assert!(
            !std::path::Path::new(&outside).exists(),
            "outside write must be denied under bwrap, same guarantee as Seatbelt"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// The `#[cfg(...)]` gate attached to the top-level definition of `name`,
    /// or `None` when that definition carries no gate. Reads this module's own
    /// source, so it sees the gates as written — including the ones the `test`
    /// cfg would otherwise hide from a running test binary.
    fn cfg_gate_above(src: &str, name: &str) -> Option<String> {
        let lines: Vec<&str> = src.lines().collect();
        let def = format!("fn {name}(");
        // Top-level definitions start at column 0; call sites are indented.
        let at = lines.iter().position(|l| l.starts_with(&def))?;
        lines[..at]
            .iter()
            .rev()
            .take_while(|l| {
                let t = l.trim_start();
                t.starts_with("#[") || t.starts_with("//") || t.is_empty()
            })
            .find(|l| l.trim_start().starts_with("#[cfg("))
            .map(|l| (*l).to_owned())
    }

    /// COX-B006 regression: a helper that is only *called* from inside a
    /// `#[cfg(target_os = ...)]` block must carry a matching gate itself.
    /// Ungated, it is compiled — and unreferenced — on every other platform,
    /// where the workspace's `warnings = "deny"` (Cargo.toml) promotes the
    /// resulting `dead_code` warning to a hard compile error.
    ///
    /// That failure is invisible to `cargo test` (the `test` cfg keeps the
    /// item alive) and to a macOS dev box, but it breaks `cargo build
    /// --release` inside the Linux Docker builder — this repo's only
    /// documented deploy path — so nothing ever answers on the published
    /// port. Asserting on the source keeps the guard honest from any host.
    #[test]
    fn platform_only_helpers_are_cfg_gated() {
        const SRC: &str = include_str!("proc.rs");
        for (name, target) in [
            ("seatbelt_profile", "macos"),
            ("bwrap_args", "linux"),
            ("bwrap_available", "linux"),
            ("current_uid", "linux"),
            // Gated for both platforms it supports.
            ("confined_command", "macos"),
            ("confined_command", "linux"),
        ] {
            let gate = cfg_gate_above(SRC, name).unwrap_or_else(|| {
                panic!(
                    "`fn {name}` is platform-specific but has no #[cfg(...)] gate: \
                     it becomes dead code on other targets and `warnings = \"deny\"` \
                     fails the Docker (Linux) release build"
                )
            });
            let want = format!("target_os = \"{target}\"");
            assert!(
                gate.contains(&want),
                "`fn {name}` must be gated on {want}, found: {gate}"
            );
        }
    }

    /// The base allowlist (work_dir + tool caches) is a pure function of
    /// `work_dir` and `home` — testable without touching the real `$HOME`,
    /// and reused by the fresh-host pre-create step in `confined_command`.
    #[test]
    fn sandbox_writable_base_includes_workdir_and_home_caches() {
        let home = std::path::PathBuf::from("/home/fakeuser");
        let list = sandbox_writable_base(std::path::Path::new("/srv/proj"), &home);
        assert!(list.contains(&std::path::PathBuf::from("/srv/proj")));
        assert!(list.contains(&home.join(".cargo")));
        assert!(list.contains(&home.join(".npm")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_group_reaps_grandchildren_too() {
        let dir = std::env::temp_dir().join(format!("cox-kg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("bg.pid");
        // sh (group leader via nice) spawns a background sleep (grandchild),
        // records its pid, then waits — exactly the shape of an agent leaving
        // a dev server running.
        let mut child = low_priority("sh")
            .arg("-c")
            .arg(format!("sleep 30 & echo $! > {}; wait", pidfile.display()))
            .spawn()
            .expect("spawn");
        // Wait for the grandchild pid to be written.
        for _ in 0..50 {
            if pidfile.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let bg: u32 = std::fs::read_to_string(&pidfile)
            .expect("pidfile")
            .trim()
            .parse()
            .expect("pid");
        let leader = child.id().expect("leader pid");
        kill_group(leader);
        // Leader must exit promptly…
        let waited = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        assert!(waited.is_ok(), "group leader must die after kill_group");
        // …and the grandchild must be gone too (signal 0 probe fails).
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let alive = std::process::Command::new("/bin/kill")
            .args(["-0", &bg.to_string()])
            .status()
            .is_ok_and(|s| s.success());
        assert!(!alive, "grandchild {bg} must be dead");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
