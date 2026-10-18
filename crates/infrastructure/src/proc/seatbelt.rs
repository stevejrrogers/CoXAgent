//! macOS Seatbelt apply-failure policy: retry `sandbox-exec` when the OS
//! refuses to apply the profile, and report honestly when it refuses every
//! time.
//!
//! `sandbox-exec` applies the compiled profile via `sandbox_apply()`, a
//! private, undocumented Apple call, from inside the already-spawned
//! `sandbox-exec` process before it execs the target program. That call
//! returns `EPERM` intermittently with no allow-list or environment difference
//! from a call that just succeeded (COX-B013). Retrying recovers the isolated
//! denial; it does NOT recover the burst case, where every call from a given
//! process fails for a stretch — measured on this repo's own dev hosts at
//! ~40% of runs (COX-B016).
//!
//! So retrying is only half a policy. The other half is telling the caller
//! WHICH happened: when every attempt is refused, the confined program never
//! ran, and calling that run [`SandboxStatus::Confined`] would report a
//! confinement that never took effect while an OS failure gets read as the
//! agent's own exit status. These helpers return the confinement ACTUALLY in
//! force — [`SandboxStatus::Denied`] once the retries are exhausted.

use coxagent_application::ports::outbound::SandboxStatus;
use tokio::process::Command;

/// How many times a macOS Seatbelt spawn is attempted in total when
/// `sandbox-exec` itself fails to even start the confined program (see
/// [`is_transient_seatbelt_apply_failure`]).
pub(super) const SEATBELT_APPLY_RETRIES: u32 = 3;

/// How long a streaming spawn waits to see whether `sandbox-exec` already
/// bailed. A real agent CLI run never completes this fast (it's a network call
/// to an LLM), so the wait costs nothing on the success path.
const SEATBELT_PROBE_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

/// True when `code` is `sandbox-exec`'s own exit status for failing to apply
/// its Seatbelt profile — NOT the target program's exit status (COX-B013).
///
/// The failure fires before the target program ever runs, so it always exits
/// with `sandbox-exec`'s own `EX_OSERR` (71) and empty stdout — a real target
/// program exiting 71 with nothing written is not a case this codebase's
/// engines (`claude`, `opencode`, `hermes`, `copilot`) produce, so the
/// signature is safe to treat as unambiguous.
pub(super) fn is_transient_seatbelt_apply_failure(
    sandbox: SandboxStatus,
    code: Option<i32>,
) -> bool {
    sandbox == SandboxStatus::Confined("seatbelt") && code == Some(71)
}

/// The confinement actually in force once the mechanism named by `sandbox` has
/// refused to apply on every attempt: the same mechanism, reported as
/// [`SandboxStatus::Denied`] so no caller mistakes a run that never happened
/// for a confined one. Pure, so the downgrade is testable on any host.
fn denied(sandbox: SandboxStatus) -> SandboxStatus {
    match sandbox {
        SandboxStatus::Confined(via) => SandboxStatus::Denied(via),
        other => other,
    }
}

/// Run `cmd` to completion via [`Command::output`], retrying when macOS
/// Seatbelt failed to even start the confined program (COX-B013). Safe to
/// retry unconditionally on that signature: nothing has run yet when it
/// fires, so a fresh spawn has no prior attempt's side effects to undo.
///
/// Returns the output together with the confinement that was actually in
/// force: [`SandboxStatus::Denied`] when every attempt was refused (the
/// command never ran), otherwise the requested status unchanged.
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
            break;
        }
        out = cmd.output().await?;
    }
    if is_transient_seatbelt_apply_failure(sandbox, out.status.code()) {
        warn_seatbelt_apply_exhausted();
        return Ok((out, denied(sandbox)));
    }
    Ok((out, sandbox))
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
         the agent's own exit status (COX-B013/COX-B016)"
    );
}

/// Spawn `cmd` for streaming (live-tailed stdout), retrying the same
/// transient Seatbelt failure `output_confined` retries (COX-B013). Briefly
/// waiting to see whether the child already exited with `sandbox-exec`'s own
/// failure signature does not touch the child's stdout/stderr — those stay
/// untouched for the caller to stream exactly as if this were a plain
/// first-try spawn.
///
/// Returns the child together with the confinement actually in force, exactly
/// as [`output_confined`] does.
///
/// # Errors
/// The underlying spawn error, after the Seatbelt retry has also failed.
pub async fn spawn_confined(
    cmd: &mut Command,
    sandbox: SandboxStatus,
) -> std::io::Result<(tokio::process::Child, SandboxStatus)> {
    spawn_confined_within(cmd, sandbox, SEATBELT_PROBE_GRACE).await
}

/// [`spawn_confined`] with the probe window injected. Only the window varies:
/// tests need one that cannot be lost to scheduling noise on a loaded host,
/// while production wants the shortest wait that still catches the failure.
async fn spawn_confined_within(
    cmd: &mut Command,
    sandbox: SandboxStatus,
    grace: std::time::Duration,
) -> std::io::Result<(tokio::process::Child, SandboxStatus)> {
    for _ in 1..SEATBELT_APPLY_RETRIES {
        let mut child = cmd.spawn()?;
        if !refused_to_apply(&mut child, sandbox, grace).await {
            return Ok((child, sandbox));
        }
    }
    // Last attempt: probe it too, so an exhausted retry loop is reported as
    // the OS refusal it is instead of handing back a child the caller will
    // read as a failed agent. The extra wait only lands on a path where every
    // earlier attempt already failed.
    let mut child = cmd.spawn()?;
    if refused_to_apply(&mut child, sandbox, grace).await {
        warn_seatbelt_apply_exhausted();
        return Ok((child, denied(sandbox)));
    }
    Ok((child, sandbox))
}

/// Whether `child` has ALREADY exited with `sandbox-exec`'s own apply-failure
/// signature, waited for at most `grace`. Only waits on the exit status — the
/// child's stdout/stderr are left untouched for the caller to stream.
async fn refused_to_apply(
    child: &mut tokio::process::Child,
    sandbox: SandboxStatus,
    grace: std::time::Duration,
) -> bool {
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(exit_status)) => is_transient_seatbelt_apply_failure(sandbox, exit_status.code()),
        // Still running (the normal case) or un-waitable: not the signature.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A script that exits with `sandbox-exec`'s own code the first
    /// `SEATBELT_APPLY_RETRIES - 1` times it runs and then succeeds — the shape
    /// a genuinely transient `sandbox_apply()` denial has — counting its runs
    /// in `counter` so a test can assert how many spawns happened.
    fn transient_then_ok_script(counter: &std::path::Path) -> String {
        format!(
            "n=$(cat {0}); n=$((n+1)); echo $n > {0}; [ $n -ge {1} ] && exit 0; exit 71",
            counter.display(),
            SEATBELT_APPLY_RETRIES
        )
    }

    /// A script that counts its runs and always exits 71 — a host stuck in the
    /// burst failure mode, where no number of retries helps.
    fn always_fails_script(counter: &std::path::Path) -> String {
        format!(
            "n=$(cat {0}); n=$((n+1)); echo $n > {0}; exit 71",
            counter.display()
        )
    }

    fn counter_file(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("cox-{tag}-{}", std::process::id()));
        std::fs::write(&p, "0").unwrap();
        p
    }

    fn runs(counter: &std::path::Path) -> String {
        std::fs::read_to_string(counter).unwrap().trim().to_owned()
    }

    #[test]
    fn denied_names_the_mechanism_that_refused_and_leaves_other_statuses_alone() {
        assert_eq!(
            denied(SandboxStatus::Confined("seatbelt")),
            SandboxStatus::Denied("seatbelt")
        );
        assert_eq!(
            denied(SandboxStatus::Unavailable("bwrap not found on PATH")),
            SandboxStatus::Unavailable("bwrap not found on PATH")
        );
        assert_eq!(
            denied(SandboxStatus::NotRequested),
            SandboxStatus::NotRequested
        );
    }

    #[tokio::test]
    async fn output_confined_retries_the_seatbelt_apply_signature_until_it_succeeds() {
        let counter = counter_file("retry");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(transient_then_ok_script(&counter));
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

    /// COX-B016: when every attempt is refused the confined program never ran,
    /// so the returned status must say so. Reporting `Confined` here claims a
    /// confinement that never took effect, and leaves the caller reading an OS
    /// failure as the agent's own exit code.
    #[tokio::test]
    async fn output_confined_reports_denied_when_every_attempt_is_refused() {
        let counter = counter_file("retry-denied");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(always_fails_script(&counter));
        let (out, sandbox) = output_confined(&mut cmd, SandboxStatus::Confined("seatbelt"))
            .await
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(71),
            "exhausted retries must surface the failure, not hang or fabricate success"
        );
        assert_eq!(
            sandbox,
            SandboxStatus::Denied("seatbelt"),
            "a run the OS never let start is not a confined run"
        );
        assert_eq!(runs(&counter), SEATBELT_APPLY_RETRIES.to_string());
        let _ = std::fs::remove_file(&counter);
    }

    #[tokio::test]
    async fn output_confined_does_not_retry_when_not_seatbelt_confined() {
        // An exit-71 command that's unrelated to Seatbelt (sandbox wasn't
        // requested, or confinement uses bwrap) must be treated as the
        // target program's own exit code, not retried as a spurious
        // `sandbox_apply()` failure.
        let counter = counter_file("retry-gate");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(always_fails_script(&counter));
        let (out, sandbox) = output_confined(&mut cmd, SandboxStatus::NotRequested)
            .await
            .unwrap();
        assert_eq!(out.status.code(), Some(71));
        assert_eq!(
            sandbox,
            SandboxStatus::NotRequested,
            "an unconfined command's own exit 71 must not be downgraded to Denied"
        );
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
    const TEST_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

    #[tokio::test]
    async fn spawn_confined_retries_the_seatbelt_apply_signature_until_it_succeeds() {
        let counter = counter_file("spawn-retry");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(transient_then_ok_script(&counter));
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

    /// COX-B016, streaming half: the path every real agent run takes must also
    /// hand back `Denied` rather than a child the caller reports as a confined
    /// agent that failed on its own.
    #[tokio::test]
    async fn spawn_confined_reports_denied_when_every_attempt_is_refused() {
        let counter = counter_file("spawn-denied");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(always_fails_script(&counter));
        let (mut child, sandbox) =
            spawn_confined_within(&mut cmd, SandboxStatus::Confined("seatbelt"), TEST_GRACE)
                .await
                .unwrap();
        assert_eq!(child.wait().await.unwrap().code(), Some(71));
        assert_eq!(
            sandbox,
            SandboxStatus::Denied("seatbelt"),
            "the OS refused every attempt — nothing ran confined"
        );
        assert_eq!(
            runs(&counter),
            SEATBELT_APPLY_RETRIES.to_string(),
            "the cap bounds the total attempts, including the last probed one"
        );
        let _ = std::fs::remove_file(&counter);
    }

    #[tokio::test]
    async fn spawn_confined_does_not_retry_when_not_seatbelt_confined() {
        let counter = counter_file("spawn-gate");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(always_fails_script(&counter));
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
