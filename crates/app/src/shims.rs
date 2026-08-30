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
pub(crate) fn shim_dir_for_process(temp_dir: &Path, pid: u32) -> PathBuf {
    temp_dir.join(format!("coxagent-shims-{pid}"))
}

/// The name prefix every shim directory this program ever wrote shares: the
/// pre-CXA-B109 shared dir (`coxagent-shims`) and the per-instance
/// `coxagent-shims-<pid>` ones alike.
const SHIM_DIR_PREFIX: &str = "coxagent-shims";

/// The pid a shim-directory name was created for, when it is one of ours.
/// `None` for the legacy shared dir, a foreign lookalike, or a malformed
/// suffix — the caller spares everything it cannot attribute to a process.
#[must_use]
fn shim_dir_pid(name: &str) -> Option<u32> {
    name.strip_prefix(SHIM_DIR_PREFIX)?
        .strip_prefix('-')?
        .parse()
        .ok()
}

/// CXA-B119: which of `names` (read from `temp_dir` by the caller) are shim
/// directories of processes that are no longer alive. Pure — the caller does
/// the reading and the `ps` call, so the decision is testable without
/// touching the real temp dir. The legacy shared `coxagent-shims` dir is
/// always stale: nothing since CXA-B109 writes it, and a hypothetical
/// pre-CXA-B109 process whose shims vanish merely degrades to unshimmed
/// tools — the same safe fallback its scripts never had.
#[must_use]
fn stale_shim_dirs(temp_dir: &Path, names: &[String], live_pids: &[u32]) -> Vec<PathBuf> {
    names
        .iter()
        .filter_map(|name| {
            if name.as_str() == SHIM_DIR_PREFIX {
                return Some(temp_dir.join(name));
            }
            let pid = shim_dir_pid(name)?;
            (!live_pids.contains(&pid)).then(|| temp_dir.join(name))
        })
        .collect()
}

/// Every pid on the host, from one `ps` call. `None` when `ps` fails — the
/// caller must then spare every directory rather than guess about liveness.
fn live_pids() -> Option<Vec<u32>> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid="])
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter_map(|token| token.parse().ok())
            .collect()
    })
}

/// CXA-B119: delete the shim directories of dead instances — every coxagent
/// start used to leak one more `coxagent-shims-<pid>` dir into temp, and
/// nothing ever removed them (179 were counted on one host). Runs once here,
/// before this instance creates its own directory: dirs are only ever created
/// by a coxagent start (see `enable_command_shims`), so the start that creates
/// is also the one that cleans — no background loop needed. Best-effort: an
/// unreadable temp dir or a failed `ps` spares everything, and a dir another
/// concurrently starting instance already removed just errors away.
fn prune_stale_shim_dirs() -> usize {
    let temp_dir = std::env::temp_dir();
    let Some(live) = live_pids() else {
        return 0; // cannot tell live from dead — do not guess
    };
    let Ok(entries) = std::fs::read_dir(&temp_dir) else {
        return 0;
    };
    let names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(SHIM_DIR_PREFIX))
        .collect();
    let stale = stale_shim_dirs(&temp_dir, &names, &live);
    for dir in &stale {
        let _ = std::fs::remove_dir_all(dir);
    }
    stale.len()
}

pub(crate) fn setup_command_shims() -> Option<PathBuf> {
    let pruned = prune_stale_shim_dirs();
    if pruned > 0 {
        tracing::info!("pruned {pruned} stale shim directories from temp");
    }
    let exe = std::env::current_exe().ok()?;
    let dir = shim_dir_for_process(&std::env::temp_dir(), std::process::id());
    std::fs::create_dir_all(&dir).ok()?;
    let dir_disp = dir.display().to_string();
    let exe_disp = exe.display().to_string();
    let mut all_written = true;
    for cmd in SHIM_CMDS {
        let script = shim_script(cmd, &dir_disp, &exe_disp);
        let p = dir.join(cmd);
        if std::fs::write(&p, script).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
        } else {
            all_written = false;
        }
    }
    // Advertise the directory only if every wrapper in it is ours: a recycled
    // pid can land this instance on a FOREIGN stale dir (shared temp dir,
    // another user's hub), and putting scripts we could not write on agent
    // PATH resurrects exactly the stale-shim failure CXA-B109 fixes. Skipping
    // the advertisement degrades to unshimmed tools — safe, never fatal.
    all_written.then_some(dir)
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

#[cfg(test)]
mod shim_prune_tests {
    use super::{live_pids, stale_shim_dirs, SHIM_DIR_PREFIX};
    use std::path::Path;

    /// CXA-B119: the sweep must delete exactly the directories of dead
    /// instances — a live hub's dir (another instance or this very process)
    /// is someone's working shims, and a name we cannot attribute to a pid of
    /// ours is never ours to remove.
    #[test]
    fn the_sweep_selects_only_dead_instances_directories() {
        let temp = Path::new("/tmp"); // never touched — pure decision over names
        let names = vec![
            "coxagent-shims-100".to_owned(),          // dead instance
            "coxagent-shims-200".to_owned(),          // live instance
            SHIM_DIR_PREFIX.to_owned(),               // legacy shared dir
            "coxagent-shims-old".to_owned(),          // malformed suffix
            "coxagent-shims-".to_owned(),             // no pid at all
            "coxagent-shims-999999999999".to_owned(), // not a u32
            "unrelated".to_owned(),                   // not ours
        ];
        let stale = stale_shim_dirs(temp, &names, &[200]);
        assert_eq!(
            stale,
            vec![
                Path::new("/tmp/coxagent-shims-100").to_path_buf(),
                Path::new("/tmp/coxagent-shims").to_path_buf(),
            ],
            "only the dead instance's dir and the abandoned legacy dir may go"
        );
    }

    /// A recycled pid lands a new instance on a dead instance's leftovers;
    /// the sweep must spare it (its pid is live) so `setup_command_shims`
    /// rewrites the scripts in place instead of racing its own removal.
    #[test]
    fn the_sweep_spares_a_recycled_pid_s_leftovers() {
        let temp = Path::new("/tmp");
        let mine = format!("{}-{}", SHIM_DIR_PREFIX, std::process::id());
        let stale = stale_shim_dirs(temp, &[mine.clone()], &[std::process::id()]);
        assert!(
            stale.is_empty(),
            "this process's own pid is by definition live — got {stale:?}"
        );
    }

    /// The adapter's liveness source must actually work where the sweep runs:
    /// a `ps` listing that omits the asking process would make the sweep see
    /// every dir as dead.
    #[test]
    fn live_pids_includes_this_process() {
        let live = live_pids().expect("ps must work where the suite runs");
        assert!(
            live.contains(&std::process::id()),
            "a live ps listing must contain the asking process"
        );
    }
}
