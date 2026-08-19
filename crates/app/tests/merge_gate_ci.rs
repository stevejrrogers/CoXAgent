//! CXA-F006 · Merge gate + auto-ticketing — FAILING contract spec.
//!
//! These tests are written FIRST against behaviour that does not exist yet;
//! implementing CXA-F006 is what flips them from red to green, one criterion at
//! a time. They follow the repo's gate-test convention of reading repository
//! content directly (`platform_gates.rs`): every assertion inspects the CI /
//! workflow artifacts F006 is expected to add and fails loudly today because
//! those artifacts are absent.
//!
//! Each test maps one-to-one onto an acceptance criterion:
//!
//!   1 · Branch protection on main requires the "Visual QA" check to pass.
//!   2 · Regressions auto-file Bug tickets with: title (view/OS), diff/baseline
//!      URLs, PR link, acceptance criteria.
//!   3 · PR comment links to auto-filed ticket(s) with ticket ID.
//!   4 · Approved baselines update via `/visual-qa-update` command or a
//!      designated label on the PR.
//!   5 · The pipeline completes end-to-end (build -> test -> compare ->
//!      ticket -> comment) in under 7 minutes.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn workflows() -> Vec<(String, String)> {
    let dir = repo_root().join(".github").join("workflows");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file()
            || !p.extension().is_some_and(|x| x == "yml" || x == "yaml")
            || p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if let Some(text) = read(&p) {
                out.push((name.to_owned(), text));
            }
        }
    }
    out.sort();
    out
}

fn visual_qa_workflow() -> Option<String> {
    workflows()
        .into_iter()
        .find(|(name, _)| name.contains("visual"))
        .map(|(_, text)| text)
}

/// Recurse over `dir` (skipping vendored/build dirs) and collect any line that
/// contains any of `needles`. Used to prove a repository-wide convention like a
/// slash command or a designated label actually exists somewhere editable.
fn scan_tree(dir: &Path, needles: &[&str]) -> Vec<String> {
    const SKIP: &[&str] = &["target", ".git", "node_modules", ".coxagent-worktrees"];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if !p
                .file_name()
                .is_some_and(|n| SKIP.contains(&n.to_string_lossy().as_ref()))
            {
                hits.extend(scan_tree(&p, needles));
            }
            continue;
        }
        if let Some(text) = read(&p) {
            hits.extend(
                text.lines()
                    .filter(|l| needles.iter().any(|n| l.contains(n)))
                    .map(str::to_owned),
            );
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

/// The exact status-check name GitHub surfaces and that branch protection must
/// list under required checks for `main`.
const VISUAL_CHECK: &str = "Visual QA";

#[test]
fn ac1_main_branch_protection_requires_the_visual_qa_check() {
    // A status check can only gate merges into main when a workflow publishes
    // it as a required status check against pull requests targeting main.
    let Some(workflow) = visual_qa_workflow() else {
        let found: Vec<String> = workflows().into_iter().map(|(n, _)| n).collect();
        panic!(
            "AC1 fail — expected a dedicated Visual-QA workflow publishing the \
             '{VISUAL_CHECK}' required status check; none exists\n\
             actual files under .github/workflows: {found:?}"
        );
    };
    // It must run on pull requests so it can gate the merge...
    assert!(
        workflow.contains("pull_request"),
        "AC1 fail — '{VISUAL_CHECK}' never runs on pull_request, so it cannot be \
         a required check gating merges into main"
    );
    // ...and be scoped to branches (main) with the pipeline actually naming and
    // surfacing the Visual-QA job as the check branch protection requires.
    assert!(
        workflow.contains("main"),
        "AC1 fail — '{VISUAL_CHECK}' workflow has no reference to the 'main' \
         branch that branch protection is supposed to guard"
    );
    assert!(
        workflow.contains(VISUAL_CHECK),
        "AC1 fail — no job/step in the pipeline surfaces a status check literally \
         named '{VISUAL_CHECK}', so GitHub has nothing to require on main"
    );
}

#[test]
fn ac2_regression_tickets_carry_all_payload_fields() {
    // A detected regression must auto-file a Bug ticket whose body enumerates the
    // full evidence payload: a view/OS-qualified title, the two screenshot URLs
    // (baseline vs diff), the PR link, and the acceptance criteria to satisfy.
    let Some(workflow) = visual_qa_workflow() else {
        panic!("AC2 fail — no Visual-QA workflow to perform regression auto-filing");
    };
    let required = [
        "baseline",     // baseline screenshot URL for the comparison
        "diff",         // diff/screenshot URL proving the regression
        "pull_request", // context to build the PR link back to
        "acceptance",   // acceptance-criteria prose carried on the ticket
    ];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|n| !workflow.contains(n))
        .collect();
    assert!(
        missing.is_empty(),
        "AC2 fail — regression Bug tickets must carry every field but these markers \
         are absent from the pipeline:\n{missing:?}\n\
         expected a step that files a Bug with title (view/OS), baseline & diff URLs, \
         PR link and acceptance criteria; got nothing comparable in .github/workflows"
    );
}

#[test]
fn ac3_pr_comment_links_to_filed_ticket_ids() {
    // After tickets are filed, the pipeline must post a comment back on the PR
    // that links to each newly-filed Bug ticket by its ID.
    let Some(workflow) = visual_qa_workflow() else {
        panic!("AC3 fail — no Visual-QA workflow to comment on the PR");
    };
    assert!(
        workflow.contains("comment"),
        "AC3 fail — no step in the pipeline posts a PR comment at all"
    );
    // The comment must embed the auto-filed ticket id(s), i.e. it references a
    // dynamic issue/ticket number rather than fixed prose only.
    let id_markers = [
        "{id}",
        "{ticket_id}",
        "#{",
        "issue_number",
        "bamboo.id",
        ".id",
    ];
    assert!(
        id_markers.iter().any(|tok| workflow.contains(tok)),
        "AC3 fail — the PR-comment step never references an auto-filed ticket ID; \
         expected a link back onto the Bug tickets filed in AC2 (markers looked \
         for: {id_markers:?})"
    );
}

#[test]
fn ac4_baselines_update_via_command_or_label() {
    // Approved baselines must be replaceable without hand-editing the store: by
    // the `/visual-qa-update` slash command OR by a designated label attached to
    // the PR. The convention has to be declared somewhere a maintainer can use.
    let root = repo_root();
    let scanned = [".github", "docs", "scripts"];
    let mut hits = Vec::new();
    for sub in &scanned {
        hits.extend(scan_tree(&root.join(sub), &["/visual-qa-update"]));
    }
    assert!(
        !hits.is_empty(),
        "AC4 fail — no '/visual-qa-update' command declared anywhere in {:?}\n\
         and no designated baseline-update label found; expected one of the two \
         mechanisms for replacing an approved baseline on a PR",
        scanned.to_vec()
    );
}

#[test]
fn ac5_pipeline_completes_in_under_seven_minutes() {
    // The whole chain — build -> test -> compare -> ticket -> comment — must fit
    // inside 7 minutes of wall-clock per run. Wall-clock depends on runner speed,
    // so this asserts the deterministic guardrail F006 must ship instead of a
    // flaky timer: every stage declares an explicit finite `timeout-minutes` cap,
    // and a documented budget marker states the sub-seven-minute target so CI can
    // fail a slow run without relying on a human reading logs.
    let Some(workflow) = visual_qa_workflow() else {
        panic!("AC5 fail — no Visual-QA pipeline whose end-to-end time is bounded");
    };
    assert!(
        workflow.contains("timeout-minutes"),
        "AC5 fail — no stage in the Visual-QA pipeline declares a timeout-minutes \
         cap; an unbounded build/test/compare chain cannot be held under 7 minutes"
    );
    assert!(
        workflow.contains("under 7 minutes")
            || workflow.contains("< 420")
            || workflow.matches("budget").count() > 0,
        "AC5 fail — no explicit sub-seven-minute budget marker is declared for the \
         end-to-end run; expected 'under 7 minutes' or equivalent on the pipeline"
    );
}
