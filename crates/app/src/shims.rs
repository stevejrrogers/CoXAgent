// Part of the composition root split by concern — see lib.rs.
#![allow(clippy::wildcard_imports)]
//! The command shims: wrappers on PATH that pipe tool output through
//! `coxagent compress`, and the byte-exact bypass for git content.

use super::*;

/// The wrapper script for one command: find the real binary on `PATH` (skipping
/// `shim_dir` so it never re-enters itself), then pipe its output through
/// `{exe} compress`, forwarding the wrapped argv so content-retrieval
/// subcommands can be recognised and left byte-exact. Pure — the caller writes
/// it, so the shape can be tested without touching the shared shim directory.
///
/// COX-B015: the pipeline merges stderr into stdout (`2>&1`) because that is
/// where cargo/npm put most of their output. For a content-retrieval `git`
/// subcommand that merge is itself corruption — a warning git wrote to stderr
/// lands in the middle of the file content, and the caller's stderr comes back
/// empty — so those bypass the pipeline entirely and `exec` the real binary.
/// Public so the COX-B015 regression test can drive the *real* script rather
/// than a hand-copied duplicate — a copy is exactly how a shim regression
/// hides from its own guard.
#[must_use]
pub fn shim_script(cmd: &str, shim_dir: &str, exe: &str) -> String {
    // Prefer exactness over token saving if the check cannot run at all: a
    // missing/broken binary must not silently re-enable the merge+compress
    // path for `git show`.
    let exact_bypass = if EXACT_AWARE_CMDS.contains(&cmd) {
        format!(
            "\x20 _exact=\"$(\"{exe}\" compress --cmd \"$cmd\" --exact-check -- \"$@\" \
             2>/dev/null)\" || _exact=\"exact\"\n\
             \x20 [ \"$_exact\" = \"exact\" ] && exec \"$real\" \"$@\"\n"
        )
    } else {
        String::new()
    };
    format!(
        "#!/usr/bin/env bash\n\
         cmd=\"{cmd}\"\n\
         real=\"\"\n\
         _IFS=\"$IFS\"; IFS=:\n\
         for d in $PATH; do\n\
         \x20 [ \"$d\" = \"{shim_dir}\" ] && continue\n\
         \x20 if [ -x \"$d/$cmd\" ]; then real=\"$d/$cmd\"; break; fi\n\
         done\n\
         IFS=\"$_IFS\"\n\
         [ -z \"$real\" ] && {{ echo \"cox-shim: $cmd not found\" >&2; exit 127; }}\n\
         if [ \"${{COX_COMPRESS:-1}}\" = \"1\" ] && [ ! -t 1 ]; then\n\
         {exact_bypass}\
         \x20 set -o pipefail\n\
         \x20 \"$real\" \"$@\" 2>&1 | \"{exe}\" compress --cmd \"$cmd\" -- \"$@\"\n\
         \x20 exit \"${{PIPESTATUS[0]:-0}}\"\n\
         fi\n\
         exec \"$real\" \"$@\"\n"
    )
}

pub(crate) fn setup_command_shims() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = std::env::temp_dir().join("coxagent-shims");
    std::fs::create_dir_all(&dir).ok()?;
    let dir_disp = dir.display().to_string();
    let exe_disp = exe.display().to_string();
    for cmd in SHIM_CMDS {
        let script = shim_script(cmd, &dir_disp, &exe_disp);
        let p = dir.join(cmd);
        if std::fs::write(&p, script).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    Some(dir)
}

/// Generate the shims and advertise them to agent subprocesses via
/// `COXAGENT_SHIM_DIR` (the engine prepends it to the child's PATH). Opt-out
/// with `COX_COMPRESS=0`. Best-effort — never fatal.
pub(crate) fn enable_command_shims() {
    if std::env::var("COX_COMPRESS").as_deref() == Ok("0") {
        return;
    }
    if let Some(dir) = setup_command_shims() {
        std::env::set_var("COXAGENT_SHIM_DIR", dir);
    }
}
