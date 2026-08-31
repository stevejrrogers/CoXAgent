//! CI workflow wiring guard (CXA-B079 + CXA-C016): the gates ci.yml declares
//! must actually run.
//!
//! The bug (B079): every job in ci.yml carried `if: false` (0387409f disabled
//! all GitHub-hosted runs while the repo was private), so the `deploy-smoke`
//! job — the "app answers on 8101" gate — never executed and availability
//! regressions shipped silently. Re-enabling it was a one-line delete, and it
//! is exactly as easy to undo. This file pins the wiring: the job exists, no
//! job-level `if:` disables it, its run step invokes the real `deploy_smoke`
//! test with `--ignored` (without that flag cargo silently runs nothing), and
//! the workflow triggers on the events the gate must cover — pushes to main
//! (post-merge verification, the ticket's whole point) and pull requests.
//!
//! C016 extends the same pin to the other two jobs: `check` (the fmt · clippy
//! · test pedantic baseline, re-enabled once its debt was paid) and
//! `deploy-build` (the linux release build the Docker builder reproduces).
//! Each must exist, be ungated, and still run its gate steps — dropping the
//! fmt step from `check` must fail here, not surface a month later as green
//! PRs over red gates.
//!
//! `why_not_wired` and `why_quality_gates_not_wired` are pure functions over
//! the workflow text that return `Err` instead of panicking. The tests at the
//! bottom feed them synthetic workflows to prove the guards actually bite:
//! re-gating a job, dropping a gate step, or losing a trigger are all caught.
//! Step-level `if: failure()` / `if: always()` conditions inside a job are
//! legitimate and must NOT read as a job gate — `job_gate` keys on the exact
//! four-space indentation of a job-level key, the edge case the real workflow
//! exercises.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

/// The CI workflow that owns the availability gate.
const CI_WORKFLOW: &str = ".github/workflows/ci.yml";

/// The job whose run step must execute the deploy smoke test.
const SMOKE_JOB: &str = "deploy-smoke";

/// The jobs whose run steps are the quality gates (C016): the pedantic
/// baseline (`fmt · clippy · test`) and the release build the Docker builder
/// reproduces. Each entry is `(job, substrings that must appear in its block)`.
const QUALITY_GATES: [(&str, &[&str]); 2] = [
    (
        "check",
        &[
            "cargo fmt --all --check",
            "cargo clippy --all-targets --all-features",
            "cargo test --all-features",
        ],
    ),
    ("deploy-build", &["cargo build --release --bin coxagent"]),
];

/// One job's YAML block: from its two-space `  <name>:` line to the next
/// job-level key or end of file. `None` when the workflow has no such job.
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

/// The job-level `if:` gate, if any. Job-level keys sit at exactly four
/// spaces; step-level conditions (`if: failure()`) sit deeper and never match
/// — a step that only runs on failure is not a disabled job.
fn job_gate(block: &str) -> Option<String> {
    block
        .lines()
        .find(|l| l.starts_with("    if:"))
        .map(|l| l.trim().to_owned())
}

/// The run step that brings the stack up — the line invoking the smoke test.
fn smoke_run(block: &str) -> Option<String> {
    block
        .lines()
        .find(|l| l.contains("--test deploy_smoke"))
        .map(|l| l.trim().to_owned())
}

/// Why would CI not verify availability after a merge? `Ok` only when the
/// deploy-smoke job exists, is ungated, runs the smoke test with `--ignored`,
/// and the workflow triggers on pushes to main and on pull requests.
fn why_not_wired(src: &str) -> Result<(), String> {
    if !src.contains("branches: [main]") || !src.contains("pull_request:") {
        return Err("workflow must trigger on pushes to main and on pull requests".to_owned());
    }
    let block =
        job_block(src, SMOKE_JOB).ok_or_else(|| format!("no `{SMOKE_JOB}` job in the workflow"))?;
    if let Some(gate) = job_gate(&block) {
        return Err(format!(
            "`{SMOKE_JOB}` is disabled by a job-level `{gate}` — the availability gate never runs"
        ));
    }
    let run =
        smoke_run(&block).ok_or_else(|| format!("`{SMOKE_JOB}` never invokes deploy_smoke"))?;
    if !run.contains("--ignored") {
        return Err(
            "the smoke test is #[ignore]d — without `--ignored` cargo runs nothing".to_owned(),
        );
    }
    Ok(())
}

/// Why would CI not enforce the quality gates after a merge? `Ok` only when
/// every [`QUALITY_GATES`] job exists, is not disabled by a job-level `if:`,
/// and still runs each of its gate steps.
fn why_quality_gates_not_wired(src: &str) -> Result<(), String> {
    for (job, steps) in QUALITY_GATES {
        let block = job_block(src, job).ok_or_else(|| format!("no `{job}` job in the workflow"))?;
        if let Some(gate) = job_gate(&block) {
            return Err(format!(
                "`{job}` is disabled by a job-level `{gate}` — the quality gates never run"
            ));
        }
        for step in steps {
            if !block.contains(step) {
                return Err(format!(
                    "`{job}` no longer runs `{step}` — a gate step was dropped"
                ));
            }
        }
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn ci_runs_the_availability_gate() {
    let path = repo_root().join(CI_WORKFLOW);
    let src = std::fs::read_to_string(&path).unwrap();
    if let Err(why) = why_not_wired(&src) {
        panic!("{CI_WORKFLOW} no longer wires up the availability gate: {why}");
    }
}

#[test]
fn ci_runs_the_quality_gates() {
    let path = repo_root().join(CI_WORKFLOW);
    let src = std::fs::read_to_string(&path).unwrap();
    if let Err(why) = why_quality_gates_not_wired(&src) {
        panic!("{CI_WORKFLOW} no longer wires up the quality gates: {why}");
    }
}

#[test]
fn a_regated_job_is_caught() {
    let src = "\
on:
  push:
    branches: [main]
  pull_request:
jobs:
  deploy-smoke:
    if: false
    name: docker compose smoke (app answers on 8101)
    steps:
      - run: cargo test -p coxagent-app --test deploy_smoke -- --ignored
";
    let why = why_not_wired(src).unwrap_err();
    assert!(why.contains("disabled by a job-level"), "unhelpful: {why}");
    assert!(why.contains("if: false"), "unhelpful: {why}");
}

#[test]
fn step_level_conditions_are_not_a_job_gate() {
    // The real workflow's shape: ungated job whose steps carry their own
    // `if: failure()` / `if: always()` conditions. Must stay Ok.
    let src = "\
on:
  push:
    branches: [main]
  pull_request:
jobs:
  deploy-smoke:
    name: docker compose smoke (app answers on 8101)
    steps:
      - run: cargo test -p coxagent-app --test deploy_smoke -- --ignored
      - name: logs
        if: failure()
        run: docker compose logs
      - name: tear down
        if: always()
        run: docker compose down -v
";
    why_not_wired(src).unwrap_or_else(|why| panic!("step conditions read as a gate: {why}"));
}

#[test]
fn dropping_ignored_runs_nothing_and_is_caught() {
    let src = "\
on:
  push:
    branches: [main]
  pull_request:
jobs:
  deploy-smoke:
    name: docker compose smoke (app answers on 8101)
    steps:
      - run: cargo test -p coxagent-app --test deploy_smoke
";
    let why = why_not_wired(src).unwrap_err();
    assert!(why.contains("--ignored"), "unhelpful: {why}");
}

#[test]
fn losing_a_trigger_is_caught() {
    let src = "\
on:
  pull_request:
jobs:
  deploy-smoke:
    name: docker compose smoke (app answers on 8101)
    steps:
      - run: cargo test -p coxagent-app --test deploy_smoke -- --ignored
";
    let why = why_not_wired(src).unwrap_err();
    assert!(why.contains("pushes to main"), "unhelpful: {why}");
}

#[test]
fn a_missing_job_is_caught() {
    let src = "\
on:
  push:
    branches: [main]
  pull_request:
jobs:
  check:
    name: fmt · clippy · test
";
    let why = why_not_wired(src).unwrap_err();
    assert!(why.contains("no `deploy-smoke` job"), "unhelpful: {why}");
}

#[test]
fn a_regated_quality_job_is_caught() {
    let src = "\
on:
  push:
    branches: [main]
  pull_request:
jobs:
  check:
    if: false
    name: fmt · clippy · test
    steps:
      - run: cargo fmt --all --check
      - run: cargo clippy --all-targets --all-features
      - run: cargo test --all-features
";
    let why = why_quality_gates_not_wired(src).unwrap_err();
    assert!(why.contains("`check` is disabled"), "unhelpful: {why}");
    assert!(why.contains("if: false"), "unhelpful: {why}");
}

#[test]
fn a_dropped_gate_step_is_caught() {
    let src = "\
on:
  push:
    branches: [main]
  pull_request:
jobs:
  check:
    name: fmt · clippy · test
    steps:
      - run: cargo fmt --all --check
      - run: cargo clippy --all-targets --all-features
";
    let why = why_quality_gates_not_wired(src).unwrap_err();
    assert!(
        why.contains("`check` no longer runs `cargo test --all-features`"),
        "unhelpful: {why}"
    );
}

#[test]
fn a_missing_quality_job_is_caught() {
    let src = "\
on:
  push:
    branches: [main]
  pull_request:
jobs:
  deploy-build:
    name: release build (linux, as the Docker builder sees it)
    steps:
      - run: cargo build --release --bin coxagent
";
    let why = why_quality_gates_not_wired(src).unwrap_err();
    assert!(why.contains("no `check` job"), "unhelpful: {why}");
}
