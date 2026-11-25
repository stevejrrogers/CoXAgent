//! CXA-B150 regression guard: a deploy build must be cut from a named ref.
//!
//! The bug: the deployed binary was built from an unmerged branch's worktree
//! while every checkout pointed elsewhere — no commit could reproduce it, and
//! QA probes against it mislead in both directions. `deploy/deploy-cxa.sh`
//! now refuses a worktree that is not a git repo, is on a detached HEAD, or
//! has uncommitted changes, and logs `branch @ sha` for every build it does
//! cut.
//!
//! These tests drive the real script against real throwaway git repos. The
//! refusal paths exit before any build or swap, so they are cheap and touch
//! nothing outside their temp directory (the deploy log is redirected via
//! `COX_DEPLOY_LOG`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn script() -> PathBuf {
    repo_root().join("deploy/deploy-cxa.sh")
}

struct TempRepo {
    root: PathBuf,
}

impl TempRepo {
    /// A bare directory — no git repo at all.
    fn plain(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("cxa-b150-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    /// A fresh git repo with one commit on branch `main` and a clean tree.
    /// The committed manifest is deliberately package-only: the deploy script's
    /// `cargo build --release --bin coxagent` must fail fast and
    /// deterministically inside the fixture (no bin target), never by walking
    /// up to whatever workspace a parent directory might hold.
    fn new(tag: &str) -> Self {
        let repo = Self::plain(tag);
        let root = &repo.root;
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "deploy guard fixture\n").unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"deploy-guard-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "fixture"]);
        repo
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Run the deploy script against this repo with the deploy log redirected.
    fn deploy(&self) -> (std::process::Output, PathBuf) {
        let log = self.root.join("deploy.log");
        let out = Command::new("bash")
            .arg(script())
            .arg(&self.root)
            .env("COX_DEPLOY_LOG", &log)
            .output()
            .expect("failed to run deploy/deploy-cxa.sh");
        (out, log)
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn combined(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The original bug: HEAD detached at some commit while the binary in the
/// field was built from a different tree entirely. The script must refuse
/// before building anything.
#[test]
fn a_deploy_from_a_detached_head_is_refused_before_any_build() {
    let repo = TempRepo::new("detached");
    repo.git(&["checkout", "--detach", "HEAD"]);

    let (out, log) = repo.deploy();

    assert!(!out.status.success(), "detached-HEAD deploy must fail");
    let text = combined(&out);
    assert!(
        text.contains("detached HEAD"),
        "refusal must name the problem, got: {text}"
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("detached HEAD"), "refusal must be logged");
    // Refused before any build or swap could start.
    assert!(!logged.contains("swapping binary"));
}

/// Uncommitted edits would bake content into the binary that no commit holds,
/// which is exactly the unreproducibility this ticket closes.
#[test]
fn a_deploy_from_a_dirty_worktree_is_refused_before_any_build() {
    let repo = TempRepo::new("dirty");
    std::fs::write(repo.root.join("README.md"), "uncommitted edit\n").unwrap();

    let (out, log) = repo.deploy();

    assert!(!out.status.success(), "dirty-tree deploy must fail");
    let text = combined(&out);
    assert!(
        text.contains("uncommitted changes"),
        "refusal must name the problem, got: {text}"
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(!logged.contains("swapping binary"));
}

/// The happy path passes the guard: the provenance line names the branch and
/// sha before the build starts. The build itself then fails fast (an empty
/// fixture repo has no Cargo.toml), which proves the guard ran first without
/// paying for a real compile.
#[test]
fn a_deploy_from_a_named_clean_ref_logs_what_it_builds() {
    let repo = TempRepo::new("named");

    let (out, log) = repo.deploy();

    assert!(
        !out.status.success(),
        "fixture build must fail (no manifest)"
    );
    let text = combined(&out);
    assert!(
        !text.contains("detached HEAD") && !text.contains("uncommitted changes"),
        "a named, clean worktree must pass the guard, got: {text}"
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    let expected = format!("building main @ {}", sha(&repo));
    assert!(
        logged.contains(&expected),
        "provenance line `{expected}` missing from log: {logged}"
    );
    // The failure came from the fixture build, before any swap could start.
    assert!(!logged.contains("swapping binary"));
}

/// The guard cannot name a ref for a directory that is not a git repo —
/// refuse rather than build something no commit could reproduce.
#[test]
fn a_deploy_from_a_non_git_directory_is_refused() {
    let dir = TempRepo::plain("nongit");

    let (out, log) = dir.deploy();

    assert!(!out.status.success(), "non-git deploy must fail");
    let text = combined(&out);
    assert!(
        text.contains("not a git worktree"),
        "refusal must name the problem, got: {text}"
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("not a git worktree"));
    assert!(!logged.contains("swapping binary"));
}

fn sha(repo: &TempRepo) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
