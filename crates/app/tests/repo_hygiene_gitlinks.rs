//! Repo hygiene: no gitlinks (mode 160000) may ever be tracked.
//!
//! `.coxagent-worktrees/*` is a runtime scratch area, but its nested repos
//! have leaked into main as gitlink entries TWICE (cleaned in 4f601b43, then
//! re-introduced by #738's squash and cleaned again in #761). `.gitignore`
//! cannot stop an entry that is already tracked on a branch and rides back in
//! through a cherry-pick or squash, so this gate fails any tree that tracks a
//! gitlink at all: the project deliberately has no submodules, so every
//! 160000 entry is a leak.

use std::process::Command;

#[test]
fn no_gitlinks_are_tracked() {
    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new("git")
        .args(["-C", root, "ls-files", "--stage"])
        .output()
        .expect("git ls-files runs");
    assert!(out.status.success(), "git ls-files failed");
    let listing = String::from_utf8_lossy(&out.stdout);
    let gitlinks: Vec<&str> = listing
        .lines()
        .filter(|l| l.starts_with("160000 "))
        .collect();
    assert!(
        gitlinks.is_empty(),
        "tracked gitlink(s) found — a nested repo leaked into the index \
         (this repo has no submodules; drop the entry with `git rm --cached`):\n{}",
        gitlinks.join("\n")
    );
}
