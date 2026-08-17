//! CXA-B063 acceptance gate: the GitHub Actions visual-regression workflow.
//!
//! CXA-B063 requires that branch protection's "Visual QA" status check be able
//! to pass — impossible while `.github/workflows/visual-qa.yml` did not exist on
//! main (only ci + desktop workflows were present). This deliverable lands that
//! workflow (authored originally as CXA-F005, but never merged) so a real job
//! emits the check. The acceptance criteria are enforced here by parsing the
//! shipped `.github/workflows/visual-qa.yml`:
//!
//!   1. Workflow exists at `.github/workflows/visual-qa.yml`
//!   2. Workflow triggers on pull_request (opened/synchronize)
//!   3. Playwright tests run in headless CI environment (Chrome, Ubuntu)
//!   4. Passing PRs: green check, no PR comment
//!   5. Failing PRs: red check with failure count, PR comment with diff images or
//!      artifact link

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

const WORKFLOW_REL: &str = ".github/workflows/visual-qa.yml";

/// Read and parse `.github/workflows/visual-qa.yml`. Deserializing straight into
/// [`serde_yaml::Value`] keeps every key exactly as authored — round-tripping via
/// JSON can mangle YAML's reserved unquoted top-level `on` trigger key.
fn load() -> serde_yaml::Value {
    let path = repo_root().join(WORKFLOW_REL);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {WORKFLOW_REL}: {e}"));
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("parse {WORKFLOW_REL}: {e}"))
}

/// The one job that owns THE green-or-red "Visual regression" check per PR.
fn single_job(doc: &serde_yaml::Value) -> &serde_yaml::Value {
    let jobs = doc["jobs"].as_mapping().expect("workflow must define jobs");
    assert_eq!(jobs.len(), 1, "{WORKFLOW_REL} must define exactly one job");
    jobs.values().next().unwrap()
}

fn steps(job: &serde_yaml::Value) -> Vec<&serde_yaml::Value> {
    job["steps"]
        .as_sequence()
        .map(|seq| seq.iter().collect())
        .unwrap_or_default()
}

/// Concatenation of every string reachable inside a step node (the action id from
/// ``uses``, an inline ``run`` shell block, and nested action inputs such as
/// github-script's inline code under ``with``), lowercased, so you can search for
/// what a step does regardless of which field carries it.
fn command(step: &serde_yaml::Value) -> String {
    fn walk(v: &serde_yaml::Value, out: &mut Vec<String>) {
        match v {
            serde_yaml::Value::String(s) => out.push(s.clone()),
            serde_yaml::Value::Mapping(m) => m.values().for_each(|c| walk(c, out)),
            serde_yaml::Value::Sequence(seq) => seq.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
    let mut parts = Vec::new();
    walk(step, &mut parts);
    parts.join("\n").to_lowercase()
}

#[test]
fn workflow_exists_and_is_valid_yaml() {
    load();
}

#[test]
fn triggers_on_pull_request_opened_and_synchronize() {
    let doc = load();
    // Note: YAML's unquoted `on:` key deserializes as the string "on".
    let types_node = doc["on"]["pull_request"]["types"]
        .as_sequence()
        .expect("must declare pull_request.types");
    let types: Vec<&str> = types_node.iter().map(|t| t.as_str().unwrap()).collect();
    assert!(types.contains(&"opened"), "must fire on opened");
    assert!(types.contains(&"synchronize"), "must fire on synchronize");
}

#[test]
fn runs_headless_chromium_on_ubuntu() {
    let doc = load();
    let job = single_job(&doc);
    let runner = job["runs-on"].as_str().expect("job must declare runs-on");
    assert!(
        runner.contains("ubuntu"),
        "headless Chrome CI needs Ubuntu, got {runner}"
    );

    // One self-contained job gives one unambiguous green-or-red check whose
    // outcome directly reflects the screenshot diff tolerance (AC#3/#4/#5).
    let joined: String = steps(job).into_iter().map(command).collect();
    assert!(
        joined.contains("playwright install chromium"),
        "must install a Chromium browser"
    );
    assert!(
        joined.contains("--update-snapshots=off"),
        "must compare against committed goldens, not regenerate them"
    );
}

/// A step's explicit condition (`if:`), lowercased; empty when unconditional.
fn condition(step: &serde_yaml::Value) -> String {
    step.get("if")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_lowercase()
}

#[test]
fn passing_prs_get_a_green_check_and_no_comment() {
    let doc = load();
    let job = single_job(&doc);
    let all_steps = steps(job);

    // Screenshots and any diff images are kept for every outcome, so a failing
    // run is still inspectable — AC#5 needs an artifact link on failure.
    let uploaders: Vec<_> = all_steps
        .iter()
        .filter(|s| command(s).contains("upload-artifact"))
        .collect();
    assert!(
        !uploaders.is_empty(),
        "must upload test-results as artifacts"
    );
    assert!(
        uploaders.iter().all(|s| condition(s).contains("always")),
        "artifacts must be uploaded on every outcome, not only failures"
    );

    // The one job is what GitHub paints green on success (AC#4 first half).
    // For the "no comment" half: any action that can write to the PR thread
    // must be gated on failure(), so a green run posts nothing.
    for s in &all_steps {
        let cmd = command(s);
        if cmd.contains("github-script")
            || cmd.contains("create-github-check")
            || cmd.contains("issues.createcomment")
            || cmd.contains("add-comment")
        {
            assert!(
                condition(s).contains("failure"),
                "a thread-posting action must be failure()-gated so passing PRs stay silent"
            );
        }
    }
}

#[test]
fn failing_prs_post_a_comment_with_count_and_diff_link() {
    let doc = load();
    let job = single_job(&doc);
    let all_steps = steps(job);

    // The single job failing is itself the red check (AC#5 first half); the
    // comment step must exist and only run when something failed, carrying both
    // a failure count and a pointer to the uploaded diff images.
    let reporters: Vec<_> = all_steps
        .iter()
        .filter(|s| condition(s).contains("failure"))
        .collect();
    assert!(
        !reporters.is_empty(),
        "there must be a reporter that runs only when the suite fails"
    );

    for s in reporters {
        let cmd = command(s);
        // github-script can both render a count and, through createComment,
        // attach or link the diff images from the artifacts.
        if cmd.contains("github-script") || cmd.contains("create-github-check") {
            assert!(
                cmd.contains("artifact") || cmd.contains("createcomment"),
                "the failing-PR reporter must reference an artifact link or post a comment"
            );
            assert!(
                cmd.contains("count") || cmd.contains("failing"),
                "the failing-PR reporter must surface how many specs failed"
            );
        }
    }
}
