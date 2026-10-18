//! CXA-B063 regression gate: protected main requires a "Visual QA" status check
//! to pass, but before this fix no workflow emitted one - so a required check
//! with no producing run failed every merge by default (and could never be made
//! green). This gate is the proof that the bug stays dead:
//!
//! 1. `.github/workflows/visual-qa.yml` must exist.
//! 2. It must declare at least one job whose CHECK RUN display name is exactly
//!    "Visual QA" (spaces intact). GitHub branch-protection matches a required
//!    status check by its CHECK RUN's display name, which comes from the job's
//!    `name:` field - or falls back to its YAML key (`visual-qa`, wrong case).
//! 3. That job must run on macOS (`macos-latest`): golden screenshots are
//!    committed as *-darwin.png, and Linux looks for *-linux.png and fails every
//!    screenshot ("missing snapshot") - an un-passable required check.
//! 4. The workflow must trigger on pull_request AND push targeting `main`.
//!
//! Checks are pure string predicates over raw workflow text so they compile and
//! behave identically everywhere; synthetic fixtures prove each rule bites before
//! the guard is pointed at the real tree.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

const WORKFLOW_REL: &str = ".github/workflows/visual-qa.yml";

/// A single unsatisfied branch-protection invariant.
#[derive(Debug)]
struct Finding {
    file: String,
    why: String,
}

fn finding(file: &str, why: String) -> Finding {
    Finding {
        file: file.to_string(),
        why,
    }
}

/// Whether any job in this workflow declares `name:` exactly "Visual QA".
fn has_visual_qa_named_job(src: &str) -> bool {
    src.lines().any(|l| l.trim() == "name: Visual QA")
}

/// Whether any job runs on macOS.
fn has_macos_job(src: &str) -> bool {
    src.lines()
        .any(|l| l.trim_start().starts_with("runs-on:") && l.contains("macos"))
}

/// Whether pull_request/push both target main.
fn triggers_main(src: &str) -> bool {
    src.contains("pull_request")
        && src.contains("push")
        && src.contains("branches")
        && src.contains("main")
}

/// Validate a whole workflow document. Empty findings == healthy.
fn check_workflow(path: &str, src: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    if !has_visual_qa_named_job(src) {
        out.push(finding(
            path,
            "no job emits a check run named \"Visual QA\" - add a job with \
             `name: Visual QA` (the spaces are significant)"
                .to_string(),
        ));
    }
    if !has_macos_job(src) {
        out.push(finding(
            path,
            "no job runs on macOS; visual goldens are committed as *-darwin.png \
             so only macOS can pass them"
                .to_string(),
        ));
    }
    if !triggers_main(src) {
        out.push(finding(
            path,
            "workflow must trigger on pull_request and push targeting main so \
             merges are actually gated"
                .to_string(),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Real-file helpers.
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Guard run against the real workflow - must pass clean after the fix.
// ---------------------------------------------------------------------------

#[test]
fn visual_qa_workflow_satisfies_branch_protection() {
    // Without this fix there is no file at all; read_file panics, failing the
    // test - which is exactly what proves the bug would be caught again.
    let src = read_file(WORKFLOW_REL);
    let findings = check_workflow(WORKFLOW_REL, &src);
    assert!(
        findings.is_empty(),
        "{WORKFLOW_REL} does not satisfy protected main (CXA-B063):\n{}",
        findings
            .iter()
            .map(|f| format!("  [{}]  {}", f.file, f.why))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------------
// Synthetic fixtures - prove each rule bites independently.
// ---------------------------------------------------------------------------

#[test]
fn a_job_named_visual_qa_is_accepted() {
    let src = "name: Visual QA\non:\n  pull_request:\n    branches: [main]\n\
               \x20 push:\n\x20   branches: [main]\njobs:\n  vqa:\n\
               \x20   runs-on: macos-latest\n";
    assert!(
        has_visual_qa_named_job(src),
        "a job named `Visual QA` must be accepted"
    );
}

#[test]
fn missing_named_job_is_caught() {
    // Only ci + desktop workflows existed before the fix: no job emits Visual QA.
    let src = "name: CI\njobs:\n  check:\n\x20   runs-on: ubuntu-latest\n";
    assert!(
        !has_visual_qa_named_job(src),
        "CI without a Visual QA job must be flagged"
    );
}

#[test]
fn linux_only_run_is_caught() {
    let src = "name: Visual QA\njobs:\n  vqa:\n\x20   runs-on: ubuntu-latest\n";
    assert!(
        !has_macos_job(src),
        "a non-macOS runner cannot pass darwin goldens"
    );
}

#[test]
fn missing_main_push_trigger_is_caught() {
    let src = "name: Visual QA\njobs:\n  vqa:\n\x20   runs-on: macos-latest\n";
    assert!(
        !triggers_main(src),
        "must trigger on pull_request and push to main"
    );
}
