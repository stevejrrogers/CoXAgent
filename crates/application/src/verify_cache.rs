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

/// Tree states a boot check is running for RIGHT NOW, per work dir.
static IN_FLIGHT: LazyLock<Mutex<HashMap<PathBuf, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Claim the boot check for this tree state, or find another runner already
/// doing it.
///
/// With `concurrency: 3` every runner reached the boot check at the same
/// moment, missed the (still empty) green cache, and started the SAME
/// `cargo test` — three compiles of one workspace, competing for the same
/// cores, each slower for the company of the others. Only the claimant runs;
/// the others skip and take the next cycle's cache hit.
///
/// The claim is per WORK DIR, not per fingerprint. Keying it on the tree state
/// let two runners through whenever their snapshots differed by a stray mtime —
/// which, in a directory the agents are actively writing to, is most of the
/// time. There is one working tree; verifying it twice at once is waste
/// whatever the second runner thinks it saw.
///
/// `false` means someone else holds it.
#[must_use]
pub fn claim_verify(work_dir: &Path, fp: Option<&str>) -> bool {
    let Ok(mut m) = IN_FLIGHT.lock() else {
        return true;
    };
    if m.contains_key(work_dir) {
        return false;
    }
    m.insert(work_dir.to_path_buf(), fp.unwrap_or(UNKNOWN).to_owned());
    true
}

/// Stands in for the fingerprint when the caller could not establish one, so a
/// claim taken without a tree state can still be released by its holder.
const UNKNOWN: &str = "\0unknown";

/// Release the claim taken by [`claim_verify`], whatever the outcome — a
/// failed or timed-out check must not wedge the gate shut forever.
pub fn release_verify(work_dir: &Path, fp: Option<&str>) {
    if let Ok(mut m) = IN_FLIGHT.lock() {
        if m.get(work_dir).map(String::as_str) == Some(fp.unwrap_or(UNKNOWN)) {
            m.remove(work_dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_runner_verifies_a_given_tree_state() {
        let dir = Path::new("/w3");
        let fp = fingerprint("head", &[]);
        assert!(claim_verify(dir, Some(&fp)), "first runner claims it");
        assert!(!claim_verify(dir, Some(&fp)), "the others stand aside");
        release_verify(dir, Some(&fp));
        assert!(claim_verify(dir, Some(&fp)), "released, so claimable again");
        // A runner whose snapshot differs by a stray mtime still waits: there
        // is one working tree, and one check of it is enough.
        let other = fingerprint("head2", &[]);
        assert!(!claim_verify(dir, Some(&other)));
        release_verify(dir, Some(&other));
        assert!(
            !claim_verify(dir, Some(&fp)),
            "only the holder's own release frees the claim"
        );
        release_verify(dir, Some(&fp));
        assert!(claim_verify(dir, Some(&fp)));
        release_verify(dir, Some(&fp));
    }

    #[test]
    fn a_claim_taken_without_a_fingerprint_is_still_released() {
        let dir = Path::new("/w4");
        assert!(claim_verify(dir, None));
        assert!(!claim_verify(dir, None), "the second runner waits");
        release_verify(dir, None);
        assert!(claim_verify(dir, None));
        release_verify(dir, None);
    }

    #[test]
    fn work_dirs_do_not_block_each_other() {
        let (a, b) = (Path::new("/w5"), Path::new("/w6"));
        assert!(claim_verify(a, None));
        assert!(claim_verify(b, None), "a different tree is a different check");
        release_verify(a, None);
        release_verify(b, None);
    }

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

    #[test]
    fn editing_a_file_inside_a_new_untracked_directory_changes_the_fingerprint() {
        // Regression for COX-B031: `git status --porcelain` (without
        // `--untracked-files=all`) collapses a new directory to one
        // `?? newdir/` line, so editing a file inside it produced the same
        // dirty line. The fingerprint must still change because callers now
        // pass per-file metadata (size, mtime) gathered with
        // `--untracked-files=all`, one DirtyEntry per file inside the dir.
        let before = fingerprint(
            "abc",
            &[("?? newdir/a.rs".to_owned(), Some((10, 100)))],
        );
        let after = fingerprint(
            "abc",
            &[("?? newdir/a.rs".to_owned(), Some((25, 200)))],
        );
        assert_ne!(before, after);
    }
}
