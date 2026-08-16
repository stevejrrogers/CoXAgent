//! Running a write-confined command when the OS itself is unreliable.
//!
//! `sandbox-exec` applies its compiled profile via `sandbox_apply()`, a
//! private, undocumented Apple call, from inside the already-spawned
//! `sandbox-exec` process and BEFORE it execs the target program. That call
//! can return `EPERM` with no allow-list or environment difference from a call
//! that just succeeded — an OS-level reliability gap in Seatbelt itself (it is
//! `DEPRECATED` per `man sandbox-exec`), not a misconfigured allow-list.
//!
//! Two things follow, and this module owns both:
//!   - **Retry** (COX-B013): the failure is usually transient, and nothing has
//!     run when it fires, so a fresh spawn has no side effects to undo.
//!   - **Tell the truth when it doesn't clear** (COX-B016): a run that never
//!     started must not be reported as `Confined` — that is a claim the
//!     sandbox held for work it never saw. Every entry point here returns the
//!     confinement that ACTUALLY applied, so an engine cannot stamp the
//!     requested status onto an outcome the OS refused to produce.
//!
//! There is deliberately no fall back to running unconfined: `workflow.sandbox`
//! is an explicit request to confine agent writes (COX-B002), and quietly
//! dropping it because the OS misbehaved would remove the protection exactly
//! when the host is least trustworthy. The run fails instead, tagged as the
//! infrastructure fault it is (see `faults::is_infra_fault`), so the ticket
//! that happened to be in flight is not charged for it.

use coxagent_application::ports::outbound::SandboxStatus;
use std::time::Duration;
use tokio::process::{Child, Command};

/// How many times a macOS Seatbelt spawn is retried after `sandbox-exec`
/// itself fails to even start the confined program.
const SEATBELT_APPLY_RETRIES: u32 = 3;

/// How long a streaming spawn waits to see whether `sandbox-exec` already
/// bailed. A real agent CLI run never completes this fast (it's a network call
/// to an LLM), so the wait costs nothing on the success path.
const SEATBELT_PROBE_GRACE: Duration = Duration::from_millis(300);

/// What a run reports when Seatbelt refused the profile on every attempt: the
/// agent never executed, so neither `Confined` (it wasn't) nor `Unavailable`
/// (that means "ran, unconfined") describes it.
const SEATBELT_REFUSED: SandboxStatus = SandboxStatus::Refused(
    "macOS Seatbelt refused to apply the sandbox profile (sandbox_apply: \
     Operation not permitted) — the confined command never ran",
);

/// True when `code` is `sandbox-exec`'s own exit status for failing to apply
/// its Seatbelt profile — NOT the target program's exit status (COX-B013).
///
/// The failure fires before the target program ever runs, so it always exits
/// with `sandbox-exec`'s own `EX_OSERR` (71) and empty stdout — a real target
/// program exiting 71 with nothing written is not a case this codebase's
/// engines (`claude`, `opencode`, `hermes`) produce, so the signature is safe
/// to treat as unambiguous.
fn is_transient_seatbelt_apply_failure(sandbox: SandboxStatus, code: Option<i32>) -> bool {
    sandbox == SandboxStatus::Confined("seatbelt") && code == Some(71)
}

/// Run `cmd` to completion via [`Command::output`], retrying when macOS
/// Seatbelt failed to even start the confined program (COX-B013), and
/// returning the confinement that actually applied to the returned output
/// (COX-B016).
///
/// This recovers a genuinely transient failure (`sandbox_apply()` denied
/// this one call but would accept the next). It does NOT recover a failure
/// that turns out to be tied to the calling process itself rather than the
/// individual call — observed in this repo's own CI/dev sandboxing, where
/// *every* `sandbox-exec` call made by an already-confined parent process
/// fails the same way for that process's whole lifetime, no matter how many
/// times or how far apart it's retried (nested Seatbelt confinement is
/// itself unreliable, separately from this bug). A caller that is itself
/// unconfined — the normal case for the `cox` hub — doesn't hit that case,
/// and one that does now gets [`SandboxStatus::Refused`] instead of an
/// unexplained exit 71 attributed to the agent.
///
/// # Errors
/// The underlying spawn/wait error, after the Seatbelt retry has also failed.
pub async fn output_confined(
    cmd: &mut Command,
    sandbox: SandboxStatus,
) -> std::io::Result<(std::process::Output, SandboxStatus)> {
    let mut out = cmd.output().await?;
    for _ in 1..SEATBELT_APPLY_RETRIES {
        if !is_transient_seatbelt_apply_failure(sandbox, out.status.code()) {
            return Ok((out, sandbox));
        }
        out = cmd.output().await?;
    }
    if is_transient_seatbelt_apply_failure(sandbox, out.status.code()) {
        warn_seatbelt_apply_exhausted();
        return Ok((out, SEATBELT_REFUSED));
    }
    Ok((out, sandbox))
}

/// Spawn `cmd` for streaming (live-tailed stdout), retrying the same
/// transient Seatbelt failure [`output_confined`] retries and reporting the
/// confinement that actually applied. Briefly waiting to see whether the child
/// already exited with `sandbox-exec`'s own failure signature does not touch
/// the child's stdout/stderr — those stay untouched for the caller to stream
/// exactly as if this were a plain first-try spawn.
///
/// # Errors
/// The underlying spawn error, after the Seatbelt retry has also failed.
pub async fn spawn_confined(
    cmd: &mut Command,
    sandbox: SandboxStatus,
) -> std::io::Result<(Child, SandboxStatus)> {
    spawn_confined_within(cmd, sandbox, SEATBELT_PROBE_GRACE).await
}

/// [`spawn_confined`] with the probe window injected. Only the window varies:
/// tests need one that cannot be lost to scheduling noise on a loaded host,
/// while production wants the shortest wait that still catches the failure.
async fn spawn_confined_within(
    cmd: &mut Command,
    sandbox: SandboxStatus,
    grace: Duration,
) -> std::io::Result<(Child, SandboxStatus)> {
    let mut child = cmd.spawn()?;
    for _ in 1..SEATBELT_APPLY_RETRIES {
        if !bailed_applying_the_profile(&mut child, sandbox, grace).await {
            return Ok((child, sandbox));
        }
        child = cmd.spawn()?;
    }
    // The last attempt is probed too: the caller is about to stream this
    // child's output, and a child that already died applying the profile has
    // none — say so instead of letting it read as an agent that said nothing.
    if bailed_applying_the_profile(&mut child, sandbox, grace).await {
        warn_seatbelt_apply_exhausted();
        return Ok((child, SEATBELT_REFUSED));
    }
    Ok((child, sandbox))
}

/// Whether `child` has already exited within `grace` with `sandbox-exec`'s own
/// apply-failure signature — i.e. the confined program never started.
async fn bailed_applying_the_profile(
    child: &mut Child,
    sandbox: SandboxStatus,
    grace: Duration,
) -> bool {
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(exit)) => is_transient_seatbelt_apply_failure(sandbox, exit.code()),
        // Still running (the normal case) or unwaitable — either way it is not
        // the pre-exec failure this probe looks for.
        _ => false,
    }
}

/// Say out loud that the OS — not the agent — killed the run. Without this the
/// caller sees an unexplained exit 71 and reads it as the agent CLI failing,
/// which is how COX-B013 stayed invisible for so long.
fn warn_seatbelt_apply_exhausted() {
    tracing::warn!(
        attempts = SEATBELT_APPLY_RETRIES,
        "macOS Seatbelt refused to apply the sandbox profile on every attempt \
         (sandbox-exec: sandbox_apply: Operation not permitted, exit 71) — the \
         confined command never ran; this is an OS-level Seatbelt failure, not \
         the agent's own exit status (COX-B013)"
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A script that fails with `sandbox-exec`'s own exit code the first
    /// `fail_times` runs and then succeeds, counting its runs in `counter` —
    /// the same shape a transient `sandbox_apply()` denial has, without
    /// needing Seatbelt (or macOS) to reproduce it.
    fn counting_script(counter: &std::path::Path, fail_times: u32) -> String {
        std::fs::write(counter, "0").unwrap();
        format!(
            "n=$(cat {0}); n=$((n+1)); echo $n > {0}; [ $n -gt {1} ] && exit 0; exit 71",
            counter.display(),
            fail_times
        )
    }

    /// The same counting script that never succeeds — a host stuck in a
    /// refusal window, which is what exhausts the retries.
    fn always_failing_script(counter: &std::path::Path) -> String {
        std::fs::write(counter, "0").unwrap();
        format!(
            "n=$(cat {0}); n=$((n+1)); echo $n > {0}; exit 71",
            counter.display()
        )
    }

    fn counter_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cox-{tag}-{}", std::process::id()))
    }

    fn runs(counter: &std::path::Path) -> String {
        std::fs::read_to_string(counter).unwrap().trim().to_owned()
    }

    #[tokio::test]
    async fn output_confined_retries_the_seatbelt_apply_signature_until_it_succeeds() {
        let counter = counter_path("retry");
        let script = counting_script(&counter, SEATBELT_APPLY_RETRIES - 1);
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&script);
        let (out, sandbox) = output_confined(&mut cmd, SandboxStatus::Confined("seatbelt"))
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "must keep retrying up to the cap and return the eventual success"
        );
        assert_eq!(
            sandbox,
            SandboxStatus::Confined("seatbelt"),
            "an attempt that DID apply the profile is genuinely confined"
        );
        assert_eq!(
            runs(&counter),
            SEATBELT_APPLY_RETRIES.to_string(),
            "must have spawned exactly up to the retry cap, not more"
        );
        let _ = std::fs::remove_file(&counter);
    }

    /// COX-B016: once the retries are spent, the run never happened — reporting
    /// it as `Confined("seatbelt")` claims a guarantee for work the sandbox
    /// never saw, and leaves the exit 71 indistinguishable from the agent's own
    /// failure.
    #[tokio::test]
    async fn output_confined_reports_refused_when_seatbelt_never_applied() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("exit 71");
        let (out, sandbox) = output_confined(&mut cmd, SandboxStatus::Confined("seatbelt"))
            .await
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(71),
            "exhausted retries must surface the failure, not hang or fabricate success"
        );
        assert!(
            matches!(sandbox, SandboxStatus::Refused(_)),
            "a run the OS never confined must not be reported as confined, got {sandbox:?}"
        );
    }

    /// The refusal must read as infrastructure, not as the ticket's own
    /// failure — otherwise an innocent ticket burns its attempts on a run that
    /// never started.
    #[test]
    fn the_refusal_reason_is_classified_as_an_infrastructure_fault() {
        let SandboxStatus::Refused(reason) = SEATBELT_REFUSED else {
            panic!("SEATBELT_REFUSED must be a Refused status");
        };
        assert!(coxagent_application::faults::is_infra_fault(reason));
        // And the raw line `sandbox-exec` itself prints, which is what reaches
        // the caller as stderr.
        assert!(coxagent_application::faults::is_infra_fault(
            "sandbox-exec: sandbox_apply: Operation not permitted"
        ));
    }

    #[tokio::test]
    async fn output_confined_does_not_retry_when_not_seatbelt_confined() {
        // An exit-71 command that's unrelated to Seatbelt (sandbox wasn't
        // requested, or confinement uses bwrap) must be treated as the
        // target program's own exit code, not retried as a spurious
        // `sandbox_apply()` failure.
        let counter = counter_path("retry-gate");
        let script = always_failing_script(&counter);
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&script);
        let (out, sandbox) = output_confined(&mut cmd, SandboxStatus::NotRequested)
            .await
            .unwrap();
        assert_eq!(out.status.code(), Some(71));
        assert_eq!(sandbox, SandboxStatus::NotRequested);
        assert_eq!(
            runs(&counter),
            "1",
            "must not retry a plain command failure"
        );
        let _ = std::fs::remove_file(&counter);
    }

    // COX-B022: `spawn_confined` is the half of the fix EVERY real agent run
    // goes through (claude/opencode stream their NDJSON stdout), yet the
    // original fix landed with `output_confined` tests only — so a forward-port
    // could silently drop the streaming retry and every test would still pass.
    // These pin the streaming path's policy the same way, without Seatbelt.

    /// A probe window wide enough that a loaded test host cannot make a failed
    /// attempt look like a still-running one: these tests are about the retry
    /// policy, not about how fast `/bin/sh` gets scheduled.
    const TEST_GRACE: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn spawn_confined_retries_the_seatbelt_apply_signature_until_it_succeeds() {
        let counter = counter_path("spawn-retry");
        let script = counting_script(&counter, SEATBELT_APPLY_RETRIES - 1);
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&script);
        let (mut child, sandbox) =
            spawn_confined_within(&mut cmd, SandboxStatus::Confined("seatbelt"), TEST_GRACE)
                .await
                .unwrap();
        assert!(
            child.wait().await.unwrap().success(),
            "must return the child of the attempt that actually applied the profile"
        );
        assert_eq!(sandbox, SandboxStatus::Confined("seatbelt"));
        assert_eq!(
            runs(&counter),
            SEATBELT_APPLY_RETRIES.to_string(),
            "must have spawned exactly up to the retry cap, not more"
        );
        let _ = std::fs::remove_file(&counter);
    }

    /// COX-B016, streaming half: the caller is about to tail this child's
    /// stdout, which is empty because the agent never ran. It must learn that
    /// from the status rather than reading it as a silent agent.
    #[tokio::test]
    async fn spawn_confined_reports_refused_when_seatbelt_never_applied() {
        let counter = counter_path("spawn-refused");
        let script = always_failing_script(&counter);
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&script);
        let (_child, sandbox) =
            spawn_confined_within(&mut cmd, SandboxStatus::Confined("seatbelt"), TEST_GRACE)
                .await
                .unwrap();
        assert!(
            matches!(sandbox, SandboxStatus::Refused(_)),
            "got {sandbox:?}"
        );
        assert_eq!(
            runs(&counter),
            SEATBELT_APPLY_RETRIES.to_string(),
            "the cap bounds the streaming path too — no unbounded respawning"
        );
        let _ = std::fs::remove_file(&counter);
    }

    #[tokio::test]
    async fn spawn_confined_does_not_retry_when_not_seatbelt_confined() {
        let counter = counter_path("spawn-gate");
        let script = always_failing_script(&counter);
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&script);
        let (mut child, sandbox) = spawn_confined(&mut cmd, SandboxStatus::NotRequested)
            .await
            .unwrap();
        assert_eq!(child.wait().await.unwrap().code(), Some(71));
        assert_eq!(sandbox, SandboxStatus::NotRequested);
        assert_eq!(
            runs(&counter),
            "1",
            "an unconfined command's own exit 71 is its result, not a sandbox_apply() failure"
        );
        let _ = std::fs::remove_file(&counter);
    }

    /// The retry probe waits on the child to see whether `sandbox-exec` already
    /// bailed — it must NOT read the child's pipes doing so, or the caller's
    /// live-tailed stdout arrives short (silent, and invisible to a test that
    /// only checks exit codes).
    #[tokio::test]
    async fn spawn_confined_leaves_the_child_stdout_intact_for_the_caller() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("echo streamed-line")
            .stdout(std::process::Stdio::piped());
        let (mut child, _) = spawn_confined(&mut cmd, SandboxStatus::Confined("seatbelt"))
            .await
            .unwrap();
        let mut buf = String::new();
        {
            use tokio::io::AsyncReadExt as _;
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut buf)
                .await
                .unwrap();
        }
        assert_eq!(
            buf.trim(),
            "streamed-line",
            "the probe must not consume the stream the caller tails"
        );
    }
}
