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
/// `false` means someone else holds it. Unknown states are never claimed —
/// with no fingerprint there is nothing to compare, so every runner proceeds
/// exactly as before.
#[must_use]
pub fn claim_verify(work_dir: &Path, fp: Option<&str>) -> bool {
    let (Some(fp), Ok(mut m)) = (fp, IN_FLIGHT.lock()) else {
        return true;
    };
    if m.get(work_dir).map(String::as_str) == Some(fp) {
        return false;
    }
    m.insert(work_dir.to_path_buf(), fp.to_owned());
    true
}

/// Release the claim taken by [`claim_verify`], whatever the outcome — a
/// failed or timed-out check must not wedge the gate shut forever.
pub fn release_verify(work_dir: &Path, fp: Option<&str>) {
    if let (Some(fp), Ok(mut m)) = (fp, IN_FLIGHT.lock()) {
        if m.get(work_dir).map(String::as_str) == Some(fp) {
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
        release_verify(dir, Some(&fp));
        // A different state is a different check.
        let other = fingerprint("head2", &[]);
        assert!(claim_verify(dir, Some(&fp)));
        assert!(claim_verify(dir, Some(&other)));
        release_verify(dir, Some(&fp));
        release_verify(dir, Some(&other));
    }

    #[test]
    fn an_unknown_state_never_blocks_a_runner() {
        let dir = Path::new("/w4");
        assert!(claim_verify(dir, None));
        assert!(claim_verify(dir, None));
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
}
