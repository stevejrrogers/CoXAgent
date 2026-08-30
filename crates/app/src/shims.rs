// Part of the composition root split by concern — see lib.rs.
#![allow(clippy::wildcard_imports)]
//! The command shims: wrappers on PATH that pipe tool output through
//! `coxagent compress`, and the byte-exact bypass for git content.

use std::collections::HashSet;

use super::*;

/// Prefix of the pid-suffixed shim directory a hub writes for itself
/// (`{prefix}<pid>`); the stale-dir sweep matches siblings by this name.
const SHIM_DIR_PREFIX: &str = "coxagent-shims-";

/// The pre-CXA-B109 SHARED shim directory name. Hubs no longer use it for
/// their own shims, but a pre-fix hub still advertises it to its agent
/// children, so its scripts must be rewritten (never removed) — see
/// `rewrite_shared_shim_dir`.
const LEGACY_SHARED_SHIM_DIR: &str = "coxagent-shims";

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

/// This hub's own shim directory: `temp/coxagent-shims-<pid>`. Pure so the
/// naming rule is testable without touching the real temp dir.
///
/// CXA-B109: the directory used to be the shared name `coxagent-shims`, so
/// every hub on the host wrote the same scripts and whoever wrote last decided
/// which binary path every OTHER hub's shims baked in — when that binary's
/// worktree was purged, every shimmed tool call host-wide lost its output. The
/// pid is unique among live processes, so concurrent hubs never collide, and a
/// recycled pid merely claims a dead instance's leftovers as its own.
#[must_use]
pub(crate) fn shim_dir_for_process(temp_dir: &Path, pid: u32) -> PathBuf {
    temp_dir.join(format!("{SHIM_DIR_PREFIX}{pid}"))
}

pub(crate) fn setup_command_shims() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let temp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let dir = shim_dir_for_process(&temp_dir, pid);
    let all_written = write_shim_scripts(&dir, &exe);
    reclaim_shim_dirs_of_dead_hubs(&temp_dir, pid);
    rewrite_shared_shim_dir(&temp_dir, &exe);
    // Advertise the directory only if every wrapper in it is ours: a recycled
    // pid can land this instance on a FOREIGN stale dir (shared temp dir,
    // another user's hub), and putting scripts we could not write on agent
    // PATH resurrects exactly the stale-shim failure CXA-B109 fixes. Skipping
    // the advertisement degrades to unshimmed tools — safe, never fatal.
    all_written.then_some(dir)
}

/// Write every wrapper script into `dir` and mark it executable; true only
/// when every wrapper is in place. Each script is staged as a hidden temp
/// sibling and renamed over its final name, so a concurrently executing shim
/// never reads a half-written script.
fn write_shim_scripts(dir: &Path, exe: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let dir_disp = dir.display().to_string();
    let exe_disp = exe.display().to_string();
    let mut all_written = true;
    for cmd in SHIM_CMDS {
        // No short-circuit: every wrapper is attempted even after a failure,
        // so a partial problem defuses as much of the hazard as it can while
        // the caller still sees all_written=false.
        all_written &= write_one_shim(dir, cmd, &dir_disp, &exe_disp);
    }
    all_written
}

/// Stage, chmod and atomically install one wrapper script; remove the staging
/// sibling on any failure so a broken write never litters the shim dir.
fn write_one_shim(dir: &Path, cmd: &str, dir_disp: &str, exe_disp: &str) -> bool {
    let staged = dir.join(format!(".{cmd}.coxagent-staged"));
    let mut ok = std::fs::write(&staged, shim_script(cmd, dir_disp, exe_disp)).is_ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        ok =
            ok && std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).is_ok();
    }
    ok = ok && std::fs::rename(&staged, dir.join(cmd)).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&staged);
    }
    ok
}

/// CXA-B117: reclaim what previous hub instances left in the temp dir.
///
/// A hub writes its shim dir at startup and never deletes it — every crash or
/// restart leaked one `coxagent-shims-<pid>` dir (184 were rotting on this
/// host). This hub's startup is the moment those become reclaimable: a
/// pid-suffixed sibling whose hub pid is absent from a fresh live-pid
/// snapshot is garbage and is removed.
///
/// Never fatal: a dir we cannot prove dead, cannot parse, or cannot remove
/// simply survives to the next hub start.
fn reclaim_shim_dirs_of_dead_hubs(temp_dir: &Path, my_pid: u32) {
    // Listing BEFORE the snapshot: a dir seen here was created by a hub that
    // already existed, so the snapshot can only confirm it alive — a hub born
    // between the two steps is never mistaken for dead. Everything after this
    // point consumes the taken listing; nothing re-lists the directory.
    let Ok(entries) = std::fs::read_dir(temp_dir) else {
        return;
    };
    let Some(live) = live_pid_snapshot() else {
        tracing::info!("shim hygiene: no live-pid snapshot available — reaping nothing");
        return;
    };
    let reaped = reap_listed_shim_dirs(entries, my_pid, &live);
    if reaped > 0 {
        tracing::info!("shim hygiene: reaped {reaped} shim dir(s) left by dead hub instances");
    }
}

/// Remove every listed entry the staleness decision marks as a dead hub's
/// leftover; returns how many were removed. The listing and the snapshot
/// arrive as data, so the only IO here is the removal itself.
fn reap_listed_shim_dirs(entries: std::fs::ReadDir, my_pid: u32, live: &HashSet<u32>) -> usize {
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            entry.path().is_dir() && is_stale_shim_dir(name, my_pid, live)
        })
        .filter(|entry| std::fs::remove_dir_all(entry.path()).is_ok())
        .count()
}

/// Pure decision over one temp-dir entry name and a live-pid snapshot: a shim
/// dir is stale iff it is pid-suffixed, the suffix parses as a pid, that pid
/// is not OURS (a recycled pid legitimately claims a dead instance's leftovers
/// — overwriting them is how the claim works), and the pid is not live. The
/// legacy shared dir (no suffix) and non-numeric suffixes are never ours to
/// reap.
#[must_use]
fn is_stale_shim_dir(name: &str, my_pid: u32, live: &HashSet<u32>) -> bool {
    name.strip_prefix(SHIM_DIR_PREFIX)
        .and_then(|suffix| suffix.parse::<u32>().ok())
        .is_some_and(|pid| pid != my_pid && !live.contains(&pid))
}

/// One snapshot of every live pid (`ps -A -o pid=`), so the whole sweep costs
/// a single spawn however many dirs have rotted. `None` = liveness unknown —
/// callers must treat every pid as alive and reap nothing.
fn live_pid_snapshot() -> Option<HashSet<u32>> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid="])
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect()
    })
}

/// CXA-B117: the pre-CXA-B109 hub wrote its shims into the SHARED name
/// `coxagent-shims`, and a pre-fix hub still running may be advertising that
/// directory to its agent children. Its scripts bake a compress binary path
/// with NO vanished-binary guard — the exact CXA-B109 hazard (one purged
/// worktree eats every shimmed tool call's output host-wide). Removing the
/// directory would exit 127 on every shimmed call those agents make, so a
/// rebuilt hub instead REWRITES the scripts in the current guarded format:
/// the old hub's agents keep working, and a vanished baked binary now
/// degrades to exact uncompressed output instead of no output.
///
/// Only an EXISTING directory is rewritten — minting a fresh shared dir that
/// nobody advertises would recreate the accident CXA-B109 removed. Best-effort.
fn rewrite_shared_shim_dir(temp_dir: &Path, exe: &Path) {
    let dir = temp_dir.join(LEGACY_SHARED_SHIM_DIR);
    if !dir.is_dir() {
        return; // no legacy dir — nothing to defuse, never mint one
    }
    if write_shim_scripts(&dir, exe) {
        tracing::info!(
            "shim hygiene: rewrote the legacy shared shim dir with fallback-guarded scripts"
        );
    }
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
mod shim_hygiene_tests {
    use super::{
        is_stale_shim_dir, reap_listed_shim_dirs, rewrite_shared_shim_dir, shim_dir_for_process,
        write_shim_scripts, LEGACY_SHARED_SHIM_DIR, SHIM_CMDS, SHIM_DIR_PREFIX,
    };
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    /// An isolated scan root: the sweep only ever looks at entries of the dir
    /// it is handed, so tests point it at a scratch dir instead of the real
    /// temp dir.
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cxa-shim-hygiene-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Sweep a scratch root against a caller-supplied snapshot (never live
    /// process state, so the assertion is deterministic).
    fn reap(root: &Path, my_pid: u32, live: &HashSet<u32>) -> usize {
        reap_listed_shim_dirs(std::fs::read_dir(root).unwrap(), my_pid, live)
    }

    /// CXA-B117: a pid-suffixed dir whose hub is dead is garbage and must be
    /// reaped; a live hub's dir, the legacy shared dir and unrelated entries
    /// must all survive the same sweep.
    #[test]
    fn a_dead_hubs_shim_dir_is_reaped_while_live_and_unrelated_dirs_survive() {
        let root = scratch("reap");
        std::fs::create_dir_all(root.join(format!("{SHIM_DIR_PREFIX}111"))).unwrap();
        std::fs::create_dir_all(root.join(format!("{SHIM_DIR_PREFIX}222"))).unwrap();
        std::fs::create_dir_all(root.join(LEGACY_SHARED_SHIM_DIR)).unwrap();
        std::fs::create_dir_all(root.join("unrelated-dir")).unwrap();
        std::fs::write(root.join("unrelated-file"), b"x").unwrap();

        // 111's hub is dead, 222's hub is alive, our own pid is 333.
        let live: HashSet<u32> = [222, 333].into_iter().collect();
        let reaped = reap(&root, 333, &live);

        assert_eq!(reaped, 1, "exactly the dead hub's dir is reaped");
        assert!(!root.join(format!("{SHIM_DIR_PREFIX}111")).exists());
        assert!(
            root.join(format!("{SHIM_DIR_PREFIX}222")).exists(),
            "a live hub's dir survives"
        );
        assert!(
            root.join(LEGACY_SHARED_SHIM_DIR).exists(),
            "the legacy shared dir is never reaped — a pre-fix hub may still advertise it"
        );
        assert!(root.join("unrelated-dir").exists());
        assert!(root.join("unrelated-file").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// A recycled pid: the current hub's OWN dir must never be swept even if
    /// the liveness snapshot misreports it — overwriting a dead instance's
    /// leftovers is exactly how a recycled pid claims them.
    #[test]
    fn our_own_shim_dir_is_never_reaped() {
        let root = scratch("own");
        let mine = shim_dir_for_process(&root, std::process::id());
        std::fs::create_dir_all(&mine).unwrap();

        let live: HashSet<u32> = HashSet::new(); // snapshot says nothing is alive
        reap(&root, std::process::id(), &live);

        assert!(mine.exists(), "the running hub's own shim dir must survive");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Only dirs whose suffix parses as a pid are ours to reap.
    #[test]
    fn a_suffix_that_is_not_a_pid_is_never_reaped() {
        let root = scratch("suffix");
        std::fs::create_dir_all(root.join(format!("{SHIM_DIR_PREFIX}garbage"))).unwrap();
        std::fs::create_dir_all(root.join(SHIM_DIR_PREFIX)).unwrap();

        reap(&root, 1, &HashSet::new());

        assert!(root.join(format!("{SHIM_DIR_PREFIX}garbage")).exists());
        assert!(root.join(SHIM_DIR_PREFIX).exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The pure decision, directly: pid-suffixed + dead = stale; live, own,
    /// suffixed-but-unparseable and the legacy shared name are not.
    #[test]
    fn staleness_is_decided_by_prefix_pid_and_liveness() {
        let live: HashSet<u32> = [222].into_iter().collect();
        assert!(is_stale_shim_dir("coxagent-shims-111", 333, &live));
        assert!(
            !is_stale_shim_dir("coxagent-shims-222", 333, &live),
            "live hub"
        );
        assert!(
            !is_stale_shim_dir("coxagent-shims-333", 333, &live),
            "our own pid"
        );
        assert!(
            !is_stale_shim_dir("coxagent-shims", 333, &live),
            "legacy shared name"
        );
        assert!(!is_stale_shim_dir("coxagent-shims-garbage", 333, &live));
        assert!(
            !is_stale_shim_dir("coxagent-shims-", 333, &live),
            "a suffix with no pid at all is not ours to reap (CXA-B119 case)"
        );
        assert!(
            !is_stale_shim_dir("coxagent-shims-999999999999", 333, &live),
            "a suffix that overflows u32 is not a pid (CXA-B119 case)"
        );
        assert!(!is_stale_shim_dir("unrelated", 333, &live));
    }

    /// The liveness snapshot must actually see live processes: if the `ps`
    /// invocation or its parsing ever breaks, every sweep would silently reap
    /// nothing (or worse, misjudge), so pin it to the one process we KNOW is
    /// alive — this test itself.
    #[test]
    fn the_live_pid_snapshot_sees_this_running_process() {
        let live = super::live_pid_snapshot().expect("`ps` must work on a dev/host machine");
        assert!(
            live.contains(&std::process::id()),
            "the snapshot must contain this running process"
        );
    }

    /// CXA-B117: a rebuilt hub must rewrite the legacy shared dir's pre-fix
    /// scripts (no vanished-binary guard — the CXA-B109 hazard) into the
    /// current guarded format, refreshing every wrapper and keeping it
    /// executable.
    #[test]
    fn the_legacy_shared_dir_scripts_gain_the_fallback_guard() {
        let root = scratch("rewrite");
        let legacy = root.join(LEGACY_SHARED_SHIM_DIR);
        std::fs::create_dir_all(&legacy).unwrap();
        // The pre-CXA-B109 format, as still found on this host: a pipeline
        // with no `[ -x <exe> ] || exec "$real"` guard.
        std::fs::write(
            legacy.join("python3"),
            "#!/usr/bin/env bash\nold unguarded\n",
        )
        .unwrap();

        let exe = Path::new("/opt/coxagent-hygiene-test");
        rewrite_shared_shim_dir(&root, exe);

        let script = std::fs::read_to_string(legacy.join("python3")).unwrap();
        assert!(
            script.contains(r#"[ -x "/opt/coxagent-hygiene-test" ] || exec "$real" "$@""#),
            "the rewritten script lost the vanished-binary guard:\n{script}"
        );
        assert!(
            script.contains("compress"),
            "the rewritten script lost the pipeline"
        );
        for cmd in SHIM_CMDS {
            let p = legacy.join(cmd);
            assert!(p.exists(), "{cmd} wrapper missing after the rewrite");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = std::fs::metadata(&p).unwrap().permissions().mode();
                assert_eq!(mode & 0o755, 0o755, "{cmd} wrapper is not executable");
            }
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// The rewrite is remediation for a dir a pre-fix hub left behind — it
    /// must never MINT the shared-format dir on a host that does not have one.
    #[test]
    fn a_missing_legacy_dir_is_never_created_by_the_rewrite() {
        let root = scratch("mint");

        rewrite_shared_shim_dir(&root, Path::new("/opt/coxagent-hygiene-test"));

        assert!(
            !root.join(LEGACY_SHARED_SHIM_DIR).exists(),
            "rewriting a non-existent legacy dir must not create it"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Both our own dir and the rewritten legacy dir get complete script sets:
    /// a partially-written shim dir must never be advertised (or left to
    /// defuse only part of the hazard).
    #[test]
    fn write_shim_scripts_reports_failure_when_a_wrapper_cannot_be_written() {
        let root = scratch("partial");
        let dir = shim_dir_for_process(&root, 4242);
        std::fs::create_dir_all(&dir).unwrap();
        // Squat a DIRECTORY where the `git` script must land: the rename over
        // it cannot succeed, so the write must report failure.
        std::fs::create_dir_all(dir.join("git")).unwrap();

        assert!(!write_shim_scripts(
            &dir,
            Path::new("/opt/coxagent-hygiene-test")
        ));
        assert!(
            dir.join("python3").exists(),
            "the other wrappers are still written"
        );
        assert!(
            !dir.join(".git.coxagent-staged").exists(),
            "a failed write must not leave staging litter"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
