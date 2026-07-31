//! Process-wide "last known green" cache for the workspace test suite.
//!
//! The DEV boot check runs `cargo test` before touching any ticket — cheap
//! when warm (~2s) but pointless when NOTHING changed since the last green
//! run. This cache fingerprints the working tree (HEAD commit + every dirty
//! path with its mtime/size) and lets callers skip the suite entirely when
//! the fingerprint still matches the last green one. Shared across all
//! runners in the process, so one green check per tree-state serves the
//! whole cycle (DEV-BUG, DEV-FEATURE, TEST).
//!
//! Correctness bias: any doubt = NOT green. No git repo, git errors, or an
//! unreadable dirty file all disable the cache rather than risk a stale skip.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static GREEN: LazyLock<Mutex<HashMap<PathBuf, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Fingerprint of the current tree state: HEAD sha plus, for every dirty or
/// untracked path, its status line, mtime and size (content edits that keep
/// the same status line still change mtime/size). `None` when the state
/// cannot be established — callers must then run the suite.
#[must_use]
pub fn fingerprint(work_dir: &Path) -> Option<String> {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let head = git(&["rev-parse", "HEAD"])?;
    // `--untracked-files=all` forces git to list every file inside a new
    // untracked directory instead of collapsing it to one `?? dir/` line —
    // without it, a content edit to a file inside that directory changes
    // nothing in the status output (and stat'ing the dir itself doesn't
    // move its mtime on an edit to a file inside it), so the fingerprint
    // would stay green on broken code.
    let status = git(&["status", "--porcelain", "--untracked-files=all"])?;
    let mut hasher = std::hash::DefaultHasher::new();
    head.trim().hash(&mut hasher);
    for line in status.lines() {
        line.hash(&mut hasher);
        // `XY path` — stat the path so content edits with an unchanged
        // status line still change the fingerprint.
        if let Some(path) = line.get(3..) {
            let p = work_dir.join(path.trim().trim_matches('"'));
            match std::fs::metadata(&p) {
                Ok(md) => {
                    md.len().hash(&mut hasher);
                    if let Ok(mt) = md.modified() {
                        mt.hash(&mut hasher);
                    }
                }
                // Deleted file: its absence is part of the state.
                Err(_) => "absent".hash(&mut hasher),
            }
        }
    }
    Some(format!("{:x}", hasher.finish()))
}

/// Whether the tree is unchanged since the last green suite run.
#[must_use]
pub fn is_green(work_dir: &Path) -> bool {
    let Some(fp) = fingerprint(work_dir) else {
        return false;
    };
    GREEN.lock().is_ok_and(|m| m.get(work_dir) == Some(&fp))
}

/// Record the current tree state as green (call right after a full suite
/// passed against exactly this state).
pub fn mark_green(work_dir: &Path) {
    if let (Some(fp), Ok(mut m)) = (fingerprint(work_dir), GREEN.lock()) {
        m.insert(work_dir.to_path_buf(), fp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(dir: &Path) {
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git")
                .status
                .success());
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "base",
        ]);
    }

    #[test]
    fn green_roundtrip_and_invalidation_on_change() {
        let dir = std::env::temp_dir().join(format!("cox-vc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        init_repo(&dir);
        assert!(!is_green(&dir), "nothing marked yet");
        mark_green(&dir);
        assert!(is_green(&dir), "unchanged tree stays green");
        std::fs::write(dir.join("new.rs"), "fn f() {}\n").expect("write");
        assert!(!is_green(&dir), "a new file invalidates the cache");
        mark_green(&dir);
        assert!(is_green(&dir));
        std::fs::write(dir.join("new.rs"), "fn f() { let _ = 1; }\n").expect("write");
        assert!(!is_green(&dir), "editing a dirty file invalidates too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editing_a_file_inside_a_new_untracked_directory_invalidates_the_cache() {
        let dir = std::env::temp_dir().join(format!("cox-vc-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("newdir")).expect("mkdir");
        init_repo(&dir);
        std::fs::write(dir.join("newdir/a.rs"), "fn f() {}\n").expect("write");
        mark_green(&dir);
        assert!(is_green(&dir), "freshly marked tree stays green");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(dir.join("newdir/a.rs"), "fn f() { does_not_compile( }\n").expect("write");
        assert!(
            !is_green(&dir),
            "editing a file inside an untracked directory must invalidate the cache"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_repo_is_never_green() {
        let dir = std::env::temp_dir().join(format!("cox-vc-plain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        mark_green(&dir);
        assert!(!is_green(&dir), "no git = cache disabled = must test");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
