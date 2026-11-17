//! CXA-F451 wiring guard — the nightly live-sandbox workflow must stay wired
//! and monitored. Mirrors the parsing-helper pattern of ci_availability_gate.rs:
//! assert the job exists, is NOT disabled, carries the cron + both secret envs,
//! invokes the `#[ignore]`d suite with `--ignored`, holds issue-write
//! permissions (monitored ≠ silent), and has a concurrency group. A negative
//! case proves deletion/regating fails loudly. Registered in `guard-tests`,
//! so unhooking it goes red rather than silent.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

const NIGHTLY_WORKFLOW: &str = ".github/workflows/nightly-live.yml";
const NIGHTLY_JOB: &str = "payments-live";
const LIVE_SUITE: &str = "payments_sandbox_live";

fn repo_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!("cannot read {rel} from repo root ({e}) — moved? fix this gate's path")
    })
}

/// One job's YAML block: same convention as ci_availability_gate.rs.
fn job_block(src: &str, job: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    let start = lines.iter().position(|l| *l == format!("  {job}:"))?;
    let rest = &lines[start + 1..];
    let end = rest
        .iter()
        .position(|l| l.starts_with("  ") && !l.starts_with("   ") && l.ends_with(':'))
        .unwrap_or(rest.len());
    Some(rest[..end].join("\n"))
}

fn run_payloads(block: &str) -> Vec<String> {
    block
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            let t = t.strip_prefix("- ").unwrap_or(t);
            t.strip_prefix("run:").map(|r| r.trim_start().to_owned())
        })
        .chain(
            // multi-line `run: |` blocks: collect the whole literal too
            block.split("run: |").skip(1).map(str::to_owned),
        )
        .collect()
}

// ---------------------------------------------------------------------------
// Positive guards — the nightly stays wired and monitored.
// ---------------------------------------------------------------------------

#[test]
fn nightly_workflow_exists_and_job_is_ungated() {
    let src = repo_file(NIGHTLY_WORKFLOW);
    let block = job_block(&src, NIGHTLY_JOB)
        .unwrap_or_else(|| panic!("no `{NIGHTLY_JOB}` job in {NIGHTLY_WORKFLOW}"));
    let gated = block
        .lines()
        .find(|l| l.starts_with("    if:") || l.starts_with("    continue-on-error:"));
    assert!(
        gated.is_none(),
        "`{NIGHTLY_JOB}` must never be job-gated ({gated:?}) — a gated nightly is a silent green"
    );
}

#[test]
fn nightly_has_cron_schedule_and_manual_dispatch() {
    let src = repo_file(NIGHTLY_WORKFLOW);
    assert!(src.contains("cron:"), "nightly schedule missing");
    assert!(
        src.contains("\"0 3 * * *\""),
        "expected the 03:00 UTC cron to stay pinned"
    );
    assert!(
        src.contains("workflow_dispatch"),
        "manual re-run dispatch missing"
    );
}

#[test]
fn nightly_exports_both_secrets_and_runs_the_live_suite_ignored() {
    let src = repo_file(NIGHTLY_WORKFLOW);
    assert!(
        src.contains("COXAGENT_SANDBOX_BASE_URL"),
        "base-url secret env missing"
    );
    assert!(
        src.contains("COXAGENT_SANDBOX_API_KEY"),
        "api-key secret env missing"
    );
    let Some(block) = job_block(&src, NIGHTLY_JOB) else {
        panic!("no `{NIGHTLY_JOB}` job in {NIGHTLY_WORKFLOW}");
    };
    let runs = run_payloads(&block);
    let invocation = runs
        .iter()
        .find(|r| r.contains(LIVE_SUITE))
        .unwrap_or_else(|| panic!("nightly never invokes --test {LIVE_SUITE}"));
    assert!(
        invocation.contains("--ignored") || src.contains("--ignored"),
        "the live suite is #[ignore]d — without `--ignored` cargo runs nothing and paints a false green"
    );
}

#[test]
fn nightly_has_issue_write_permissions_monitored_not_silent() {
    let src = repo_file(NIGHTLY_WORKFLOW);
    let Some(perms) = src.split("permissions:").nth(1) else {
        panic!("permissions: block missing — a failed nightly could not raise any alert");
    };
    assert!(
        perms.contains("issues: write"),
        "issue-write permission missing: a failed nightly must open/update a tracked issue"
    );
}

#[test]
fn nightly_has_a_concurrency_group_never_cancel_in_progress() {
    let src = repo_file(NIGHTLY_WORKFLOW);
    assert!(src.contains("concurrency:"), "concurrency group missing");
    assert!(
        src.contains("group: nightly-payments"),
        "expected the nightly-payments concurrency group"
    );
    let cblock = src.split("concurrency:").nth(1).unwrap_or_default();
    let cancel = cblock
        .lines()
        .find(|l| l.trim_start().starts_with("cancel-in-progress:"))
        .and_then(|l| l.split('#').next())
        .and_then(|l| l.split(':').nth(1))
        .map_or_else(|| "", str::trim);
    assert_eq!(
        cancel, "false",
        "partial nightly runs must complete to report which scenarios failed"
    );
}

#[test]
fn nightly_failure_alert_step_names_scenarios_via_tracked_issue() {
    let src = repo_file(NIGHTLY_WORKFLOW);
    assert!(
        src.contains("nightly-live-failure"),
        "tracked-issue label missing — failing-scenario attribution lives on the issue"
    );
    assert!(
        src.contains("if: failure()"),
        "failure-only step missing — monitoring must react to failure, not success"
    );
}

// ---------------------------------------------------------------------------
// Negative guards — removal / regating goes loudly red here.
// ---------------------------------------------------------------------------

#[test]
fn deleting_the_nightly_job_fails_loudly() {
    let src = repo_file(NIGHTLY_WORKFLOW);
    assert!(
        job_block(&src, NIGHTLY_JOB).is_some(),
        "`{NIGHTLY_JOB}` was deleted from {NIGHTLY_WORKFLOW} — restore it; the nightly live run is the only sandbox verification lane"
    );
}

#[test]
fn dropping_either_secret_env_fails_loudly() {
    let src = repo_file(NIGHTLY_WORKFLOW);
    for var in ["COXAGENT_SANDBOX_BASE_URL", "COXAGENT_SANDBOX_API_KEY"] {
        assert!(
            src.contains(var),
            "{var} no longer exported by the nightly job — fail-closed posture broken"
        );
    }
}

#[test]
fn renaming_the_live_suite_invocation_fails_loudly() {
    let src = repo_file(NIGHTLY_WORKFLOW);
    assert!(
        src.contains(LIVE_SUITE),
        "`{LIVE_SUITE}` no longer referenced by the nightly — the sandbox lane runs zero tests"
    );
}
