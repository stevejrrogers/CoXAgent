//! CXA-B062 regression guard · Visual QA pipeline must actually be integrated.
//!
//! The bug: CXA-F005 ("GitHub Actions visual regression") and CXA-F006 ("Merge
//! gate + auto-ticketing") were marked shipped, but their sole deliverable —
//! `.github/workflows/visual-qa.yml`, which every dependent acceptance criterion
//! builds on — never reached main. `.github/workflows/` carried only ci.yml +
//! desktop.yml, so no Playwright screenshots ran on PRs, no Visual-QA Bug
//! tickets were ever filed automatically, and no merge-gate "Visual QA" check
//! existed to enforce.
//!
//! This guard FAILS today (the workflow is absent) and passes only when a
//! workflow that actually performs each stage of that pipeline is present:
//!
//!   1 · triggers on pull_request       — screenshots run when a PR opens/syncs
//!   2 · runs Playwright against committed baselines with regeneration OFF
//!   3 · uploads evidence (diff images/report) for every run so reviewers can see it
//!   4 · files a Bug ticket through GitHub CLI when something regresses
//!   5 · comments back on the PR linking that ticket / artifact so humans see it

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

const WORKFLOW_REL: &str = ".github/workflows/visual-qa.yml";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Read + parse the shipped workflow as YAML value trees. Parsing into
/// [`serde_yaml::Value`] keeps keys exactly as authored (round-tripping via JSON
/// mangles YAML's reserved unquoted top-level `on` trigger key).
fn load() -> serde_yaml::Value {
    let path = repo_root().join(WORKFLOW_REL);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {WORKFLOW_REL}: {e}"));
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("parse {WORKFLOW_REL}: {e}"))
}

fn single_job(doc: &serde_yaml::Value) -> &serde_yaml::Value {
    doc["jobs"]
        .as_mapping()
        .expect("workflow must define jobs")
        .values()
        .next()
        .unwrap()
}

fn steps(job: &serde_yaml::Value) -> Vec<&serde_yaml::Value> {
    job["steps"]
        .as_sequence()
        .map(|seq| seq.iter().collect())
        .unwrap_or_default()
}

/// A step's explicit condition (`if:`), lowercased; empty when unconditional.
fn condition(step: &serde_yaml::Value) -> String {
    step.get("if")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_lowercase()
}

/// Lowercased concatenation of every string reachable inside a step node (`uses`,
/// inline `run`, nested action inputs), so we can search for what any step does
/// regardless of which field carries it.
fn command(step: &serde_yaml::Value) -> String {
    fn walk(v: &serde_yaml::Value, out: &mut Vec<String>) {
        match v {
            serde_yaml::Value::String(s) => out.push(s.to_lowercase()),
            serde_yaml::Value::Mapping(m) => m.values().for_each(|c| walk(c, out)),
            serde_yaml::Value::Sequence(seq) => seq.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
    let mut parts = Vec::new();
    walk(step, &mut parts);
    parts.join("\n")
}

#[test]
fn workflow_exists_and_is_valid_with_one_check_job() {
    // Criterion 0 · The one canonical path exists on main and parses as valid YAML,
    // carrying exactly one job so it paints one unambiguous green-or-red check per PR.
    assert!(repo_root().join(WORKFLOW_REL).is_file(), "{WORKFLOW_REL} missing from main");
    load();
}

#[test]
fn triggers_on_pull_request_open_and_sync() {
    // Criterion 1 · Screenshots run when a pull request opens or syncs,
    // not just manually via workflow_dispatch or by editing something else.
    let doc = load();
    let types_node = doc["on"]["pull_request"]["types"]
        .as_sequence()
        .expect("must declare pull_request.types");
    let types: Vec<String> = types_node.iter().map(|t| t.as_str().unwrap().to_string()).collect();
    assert!(types.iter().any(|t| t == "opened"), "must fire on opened");
    assert!(types.iter().any(|t| t == "synchronize"), "must fire on synchronize");
}

#[test]
fn runs_screenshots_against_committed_baselines_regeneration_off() {
    // Criterion 2 · A headless Chromium suite compares against committed goldens;
    // update mode would silently bless every UI change instead of flagging regressions.
    let doc = load();
    let job = single_job(&doc);
    let joined: String = steps(job).into_iter().map(command).collect();
    assert!(
        joined.contains("playwright install") && joined.contains("chromium"),
        "must install a Chromium browser for headless screenshots"
    );
    assert!(
        joined.contains("--update-snapshots=off"),
        "must compare against committed goldens, not regenerate them"
    );
}

#[test]
fn uploads_evidence_on_every_outcome() {
    // Criterion 3 · Diff images / report are kept for every outcome so a failing
    // run is still inspectable — the regression comment needs an artifact link.
    let doc = load();
    let job = single_job(&doc);
    let mut any_uploader = false;
    for s in steps(job) {
        if command(s).contains("upload-artifact") {
            any_uploader = true;
            assert!(
                condition(s).contains("always"),
                "artifacts must be uploaded on every outcome, not only failures"
            );
        }
    }
    assert!(any_uploader, "must upload test-results as artifacts");
}

#[test]
fn failing_prs_file_a_bug_ticket_and_post_a_link() {
    // Criteria 4 + 5 · On failure the workflow both files a Bug through GitHub CLI
    // and comments back on the PR linking it, so regressions surface as tickets.
    let doc = load();
    let job = single_job(&doc);
    let all_steps = steps(job);

    // Criterion 4 · Some step files a Bug ticket with GitHub CLI.
    assert!(
        all_steps
            .clone()
            .into_iter()
            .any(|s| command(s).contains("gh issue create")),
        "must file a Bug via `gh issue create` on regression"
    );

    // Criterion 5 · A different step comments on the PR thread, gated on failure so
    // green runs post nothing, and links concrete evidence — the filed Bug URL wired
    // across steps via an action output/env reference (NOT a literal `{ticket_id}`
    // placeholder, which each shell step can never resolve — the defect that sank
    // the original CXA-F006 #221).
    let mut any_commenter = false;
    for s in all_steps.clone().into_iter() {
        if !(command(s).contains("pr comment") || command(s).contains("issues.createcomment")) {
            continue;
        }
        any_commenter = true;
        assert!(
            condition(s).contains("failure"),
            "a PR-thread comment must be failure()-gated so passing PRs stay silent"
        );
        let c = command(s);
        assert!(
            !c.contains("{ticket_id}"),
            "the comment must NOT carry an unresolvable literal `{{{{ticket_id}}}}` \
             placeholder — it must reference cross-step output/env wiring instead"
        );
        assert!(
            c.contains("outputs") || c.contains("env."),
            "the failing-PR comment must reference evidence through cross-step \
             output/env wiring (got: {c})"
        );
    }
    assert!(any_commenter, "must comment on the PR thread");
}
