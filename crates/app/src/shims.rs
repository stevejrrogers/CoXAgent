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
///
/// CXA-B109: the compress binary's path is baked in at generation time and can
/// vanish underneath the script (a hub wrote these shims from a worktree the
/// janitor later purged). Without a guard, the pipeline's second stage dies at
/// exec and the first stage SIGPIPEs — exit 141, zero bytes of output, for
/// every shimmed tool call. Before piping, the script therefore checks the
/// baked binary and degrades to `exec "$real"`: exact, uncompressed output
/// beats no output.
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
         \x20 [ -x \"{exe}\" ] || exec \"$real\" \"$@\"\n\
         \x20 set -o pipefail\n\
         \x20 \"$real\" \"$@\" 2>&1 | \"{exe}\" compress --cmd \"$cmd\" -- \"$@\"\n\
         \x20 exit \"${{PIPESTATUS[0]:-0}}\"\n\
         fi\n\
         exec \"$real\" \"$@\"\n"
    )
}

/// One shim directory per hub instance: `temp/coxagent-shims-<pid>`. Pure so
/// the naming rule is testable without touching the real temp dir.
///
/// CXA-B109: the directory used to be the shared name `coxagent-shims`, so
/// every hub on the host wrote the same scripts and whoever wrote last decided
/// which binary path every OTHER hub's shims baked in — when that binary's
/// worktree was purged, every shimmed tool call host-wide lost its output. The
/// pid is unique among live processes, so concurrent hubs never collide, and a
/// recycled pid merely claims a dead instance's leftovers as its own.
#[must_use]
pub fn shim_dir_for_process(temp_dir: &Path, pid: u32) -> PathBuf {
    temp_dir.join(format!("coxagent-shims-{pid}"))
}

pub(crate) fn setup_command_shims() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = shim_dir_for_process(&std::env::temp_dir(), std::process::id());
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

#[cfg(test)]
mod shim_dir_tests {
    use super::shim_dir_for_process;
    use std::path::Path;

    /// CXA-B109: two hub instances must never share one shim directory — the
    /// shared `coxagent-shims` name let the last writer's baked binary path
    /// decide for every hub on the host, and a purged worktree then ate every
    /// shimmed tool call's output host-wide.
    #[test]
    fn distinct_hub_instances_get_distinct_shim_directories() {
        let temp = Path::new("/tmp");
        let a = shim_dir_for_process(temp, 100);
        let b = shim_dir_for_process(temp, 200);
        assert_ne!(a, b, "two live hubs would overwrite each other's shims");
        assert_eq!(shim_dir_for_process(temp, 100), a, "a hub must be stable");
    }

    /// The name must stay recognisable as a shim directory: the COX-B015
    /// integration guard filters these directories out of the ambient PATH
    /// when it looks for the real `git`, and an agent debugging PATH oddities
    /// should be able to spot them too.
    #[test]
    fn the_directory_name_stays_recognisable_as_a_shim_dir() {
        let dir = shim_dir_for_process(Path::new("/var/folders/x/T"), 4242);
        assert_eq!(
            dir,
            Path::new("/var/folders/x/T/coxagent-shims-4242"),
            "the pid-suffixed name must keep the coxagent-shims prefix"
        );
    }
}
