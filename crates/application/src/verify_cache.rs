//! Process-wide "last known green" cache for the workspace test suite.
//!
//! The DEV boot check runs `cargo test` before touching any ticket — cheap
//! when warm (~2s) but pointless when NOTHING changed since the last green
//! run. This cache maps a work dir to the fingerprint of the tree the last
//! green run saw; callers compute the current fingerprint (through their
//! ports — no IO happens here) and skip the suite when it still matches.
//!
//! Correctness bias: any doubt = NOT green. Callers that cannot establish
//! the tree state pass `None` and the cache stands aside.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static GREEN: LazyLock<Mutex<HashMap<PathBuf, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// One dirty/untracked path as `git status --porcelain` lists it, plus its
/// `(size, mtime_epoch)` when the file exists — `None` marks a deletion,
/// whose absence is part of the state.
pub type DirtyEntry = (String, Option<(u64, u64)>);

/// Hash the tree state: HEAD sha plus every dirty path with its metadata.
/// Content edits that keep the same status line still change mtime/size.
/// Pure — the caller gathers `head` and `dirty` through its ports.
#[must_use]
pub fn fingerprint(head: &str, dirty: &[DirtyEntry]) -> String {
    let mut hasher = std::hash::DefaultHasher::new();
    head.trim().hash(&mut hasher);
    for (line, meta) in dirty {
        line.hash(&mut hasher);
        match meta {
            Some((size, mtime)) => {
                size.hash(&mut hasher);
                mtime.hash(&mut hasher);
            }
            None => "absent".hash(&mut hasher),
        }
    }
    format!("{:x}", hasher.finish())
}

/// Whether `fp` (the current tree fingerprint, `None` = unknown) matches the
/// last green run for this work dir. Unknown is never green.
#[must_use]
pub fn is_green(work_dir: &Path, fp: Option<&str>) -> bool {
    let Some(fp) = fp else { return false };
    GREEN
        .lock()
        .is_ok_and(|m| m.get(work_dir).map(String::as_str) == Some(fp))
}

/// Record `fp` as the green tree state for this work dir (call right after a
/// full suite passed against exactly this state). Unknown states are not
/// recorded.
pub fn mark_green(work_dir: &Path, fp: Option<&str>) {
    if let (Some(fp), Ok(mut m)) = (fp, GREEN.lock()) {
        m.insert(work_dir.to_path_buf(), fp.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_state_matches_and_any_change_invalidates() {
        let dir = Path::new("/w1");
        let fp1 = fingerprint("abc", &[("M src/lib.rs".to_owned(), Some((10, 99)))]);
        mark_green(dir, Some(&fp1));
        assert!(is_green(dir, Some(&fp1)));
        // Same status line, different mtime — a content edit.
        let fp2 = fingerprint("abc", &[("M src/lib.rs".to_owned(), Some((10, 100)))]);
        assert!(!is_green(dir, Some(&fp2)));
        // A deletion is its own state.
        let fp3 = fingerprint("abc", &[("M src/lib.rs".to_owned(), None)]);
        assert!(!is_green(dir, Some(&fp3)));
        assert_ne!(fp1, fp3);
    }

    #[test]
    fn unknown_state_is_never_green_and_never_recorded() {
        let dir = Path::new("/w2");
        mark_green(dir, None);
        assert!(!is_green(dir, None));
        assert!(!is_green(dir, Some("anything")));
    }
}
