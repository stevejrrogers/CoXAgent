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
//! F286 adds the last two pins. First, a `guard-tests` job (display name
//! `ownership + CI wiring guards`) must run the reclaimable teardown-policy
//! tests and the pure deploy_smoke ownership decisions on every push and PR —
//! docker-free and ungated, so a teardown-policy regression or a docker-less
//! runner can never paint a false green. Second, both gate jobs must expose
//! exactly the check-run display names ci.yml documents for branch
//! protection on main to require as status checks — a rename would otherwise
//! silently dangle the required checks and unblock merges.
//!
//! `why_not_wired` and `why_quality_gates_not_wired` are pure functions over
//! the workflow text that return `Err` instead of panicking. The tests at the
//! bottom feed them synthetic workflows to prove the guards actually bite:
//! re-gating a job, `continue-on-error`ing it, dropping a gate step, or
//! losing a trigger are all caught. Step-level `if: failure()` /
//! `if: always()` conditions inside a job are legitimate and must NOT read
//! as a job gate — `job_gate` keys on the exact four-space indentation of a
//! job-level key, the edge case the real workflow exercises.
//!
//! F326 adds the last pin: the `integration` job is the only sanctioned way
//! to run the `#[ignore]`d DSN-gated Postgres/Redis suites, so it must exist,
//! be ungated, bring its own ephemeral postgres+redis service containers
//! (database named `cxa_test`, exactly what the shared guard demands), export
//! `COXAGENT_TEST_PG_DSN` and `COXAGENT_TEST_REDIS_URL` explicitly, and run
//! the gated suites with `--ignored` — without which cargo silently runs
//! nothing and the job paints a green over zero executed tests.

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

/// The job that makes the guard tests merge-blocking CI (F286): the
/// reclaimable teardown policy (protected infrastructure never reclaimable)
/// and the pure deploy_smoke ownership decisions, plus this file's own
/// wiring guards — ungated and docker-free, so neither a teardown-policy
/// regression nor a docker-less runner can paint a false green.
const GUARD_TESTS_JOB: &str = "guard-tests";

/// The gate commands the guard job must run, as exact run-payload substrings.
const GUARD_TESTS_STEPS: [&str; 3] = [
    "cargo test -p coxagent-infrastructure --lib reclaimable",
    "cargo test -p coxagent-app --test deploy_smoke --test ci_availability_gate \
     --test test_env_guard_f326_gate",
    // CXA-B225: the named guardrail suite is wired into the PR gate — this
    // pin makes its removal a red wiring guard, not a silent unhooking.
    "cargo test -p coxagent-presentation --test guardrail_scaffold_b201",
];

/// The job that runs the `#[ignore]`d DSN-gated integration tests (F326) —
/// the CI-level half of the fail-closed test-environment guard. It must
/// carry its own ephemeral service containers, never touch a deployed hub.
const INTEGRATION_JOB: &str = "integration";

/// The check-run display names (the jobs' `name:` fields) that branch
/// protection on main must require as status checks, so merge-blocking is
/// enactable from repo state alone (F286). GitHub matches a required status
/// check against exactly this string.
const REQUIRED_CHECKS: [(&str, &str); 2] = [
    (SMOKE_JOB, "docker compose smoke (app answers on 8101)"),
    (GUARD_TESTS_JOB, "ownership + CI wiring guards"),
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

/// The job-level disable, if any: an `if:` gate or a `continue-on-error`
/// (a failed run that still reports green — the same silent bypass).
/// Job-level keys sit at exactly four spaces; step-level conditions
/// (`if: failure()`) sit deeper and never match — a step that only runs on
/// failure is not a disabled job.
fn job_gate(block: &str) -> Option<String> {
    block
        .lines()
        .find(|l| l.starts_with("    if:") || l.starts_with("    continue-on-error:"))
        .map(|l| l.trim().to_owned())
}

/// The job's `run:` payloads (trimmed) — what would actually execute. Both
/// step spellings count: `run:` on its own line and the inline `- run:` form.
fn run_payloads(block: &str) -> Vec<String> {
    block
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            let t = t.strip_prefix("- ").unwrap_or(t);
            t.strip_prefix("run:").map(|r| r.trim().to_owned())
        })
        .collect()
}

/// The job's check-run display name: its first job-level `    name:` line.
/// GitHub matches a required status check against exactly this string.
fn job_display_name(block: &str) -> Option<String> {
    block
        .lines()
        .find_map(|l| l.strip_prefix("    name:").map(str::trim))
        .map(ToOwned::to_owned)
}

/// The workflow's comment text — where the repo documents the check-run
/// display names branch protection on main must require.
fn comment_text(src: &str) -> String {
    src.lines()
        .filter_map(|l| l.trim_start().strip_prefix('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Why would CI not verify availability after a merge? `Ok` only when the
/// deploy-smoke job exists, is ungated, runs the smoke test with `--ignored`,
/// and the workflow triggers on pushes to main and on pull requests.
fn why_not_wired(src: &str) -> Result<(), String> {
    // Cost discipline (2026-09): the contract is push-to-main (+ manual
    // dispatch), NOT per-PR — PRs are verified locally by the review lane
    // before merging, so billed minutes are spent once per change.
    if !src.contains("branches: [main]") || !src.contains("workflow_dispatch:") {
        return Err(
            "workflow must trigger on pushes to main and allow workflow_dispatch".to_owned(),
        );
    }
    let block =
        job_block(src, SMOKE_JOB).ok_or_else(|| format!("no `{SMOKE_JOB}` job in the workflow"))?;
    if let Some(gate) = job_gate(&block) {
        return Err(format!(
            "`{SMOKE_JOB}` is disabled by a job-level `{gate}` — the availability gate never runs"
        ));
    }
    let run = run_payloads(&block)
        .into_iter()
        .find(|r| r.contains("--test deploy_smoke"))
        .ok_or_else(|| format!("`{SMOKE_JOB}` never invokes deploy_smoke"))?;
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

/// Why would CI not run the guard tests (F286)? `Ok` only when the
/// `guard-tests` job exists, is ungated, and runs both gate commands — the
/// reclaimable teardown-policy tests and the pure deploy_smoke +
/// ci_availability_gate suites — without touching docker: the guard is a
/// pure decision over names, labels and workflow text, so depending on a
/// daemon would let a docker-less runner silently skip it.
fn why_guard_tests_not_wired(src: &str) -> Result<(), String> {
    let block = job_block(src, GUARD_TESTS_JOB)
        .ok_or_else(|| format!("no `{GUARD_TESTS_JOB}` job in the workflow"))?;
    if let Some(gate) = job_gate(&block) {
        return Err(format!(
            "`{GUARD_TESTS_JOB}` is disabled by a job-level `{gate}` — the \
             guard tests never run"
        ));
    }
    let runs = run_payloads(&block);
    for step in GUARD_TESTS_STEPS {
        if !runs.iter().any(|r| r.contains(step)) {
            return Err(format!(
                "`{GUARD_TESTS_JOB}` no longer runs `{step}` — a gate step \
                 was dropped"
            ));
        }
    }
    if let Some(run) = runs.iter().find(|r| r.contains("docker")) {
        return Err(format!(
            "the guard tests are pure decisions over names and labels — \
             `{run}` needs a docker daemon a runner may not have"
        ));
    }
    Ok(())
}

/// Why would merge-blocking not be enactable from repo state alone (F286)?
/// `Ok` only when every [`REQUIRED_CHECKS`] job exposes exactly its
/// documented display `name:`, and the workflow's comments document those
/// names together with the branch-protection requirement — so the required
/// checks can never silently dangle after a rename.
fn why_check_names_not_enactable(src: &str) -> Result<(), String> {
    for (job, name) in REQUIRED_CHECKS {
        let block = job_block(src, job).ok_or_else(|| format!("no `{job}` job in the workflow"))?;
        let exposed = job_display_name(&block).ok_or_else(|| {
            format!(
                "`{job}` declares no display `name:` — GitHub would fall back \
                 to the YAML key `{job}`, which cannot match the required check"
            )
        })?;
        if exposed != name {
            return Err(format!(
                "`{job}`'s check-run display name drifted: the workflow says \
                 `{exposed}` but branch protection must require `{name}`"
            ));
        }
    }
    let comments = comment_text(src);
    for (_, name) in REQUIRED_CHECKS {
        if !comments.contains(name) {
            return Err(format!(
                "ci.yml no longer documents required status check `{name}` in \
                 a comment — update the documentation together with any rename"
            ));
        }
    }
    if !comments.contains("branch protection") {
        return Err(
            "ci.yml must say the check names are what branch protection on \
             main must require as status checks"
                .to_owned(),
        );
    }
    Ok(())
}

/// A job's exported env value: the first `VAR: value` line in the block.
fn env_value(block: &str, var: &str) -> Option<String> {
    block.lines().find_map(|l| {
        l.trim()
            .strip_prefix(var)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(str::trim)
            .map(ToOwned::to_owned)
    })
}

/// Why would CI never run the DSN-gated integration tests (F326)? `Ok` only
/// when the `integration` job exists, is ungated, brings its own ephemeral
/// postgres+redis service containers, exports `COXAGENT_TEST_PG_DSN` at a
/// `cxa_test*` database plus `COXAGENT_TEST_REDIS_URL` explicitly, and runs
/// the gated suites with `--ignored` — the tests refuse to decide safety on
/// their own, so a job without any of these runs nothing (or fails loudly).
fn why_integration_tests_not_wired(src: &str) -> Result<(), String> {
    let block = job_block(src, INTEGRATION_JOB).ok_or_else(|| {
        format!(
            "no `{INTEGRATION_JOB}` job in the workflow — the #[ignore]d \
             DSN-gated integration tests would never run"
        )
    })?;
    if let Some(gate) = job_gate(&block) {
        return Err(format!(
            "`{INTEGRATION_JOB}` is disabled by a job-level `{gate}` — the \
             integration tests never run"
        ));
    }
    for service in ["image: postgres", "image: redis"] {
        if !block.contains(service) {
            return Err(format!(
                "`{INTEGRATION_JOB}` lost its `{service}` service container — \
                 the gated tests refuse to run against anything but an \
                 ephemeral target the job itself owns"
            ));
        }
    }
    let dsn = env_value(&block, "COXAGENT_TEST_PG_DSN").ok_or_else(|| {
        format!(
            "`{INTEGRATION_JOB}` no longer exports COXAGENT_TEST_PG_DSN — the \
             shared guard fails closed on an unset env, so the job would run \
             nothing but refusals"
        )
    })?;
    if !dsn.contains("cxa_test") {
        return Err(format!(
            "COXAGENT_TEST_PG_DSN must name a cxa_test* database (got `{dsn}`) — \
             the shared guard refuses anything else, and a live-hub-shaped DSN \
             is exactly what the guard exists to refuse"
        ));
    }
    if env_value(&block, "COXAGENT_TEST_REDIS_URL").is_none() {
        return Err(format!(
            "`{INTEGRATION_JOB}` no longer exports COXAGENT_TEST_REDIS_URL — the \
             coordination tests demand it explicitly and the guard fails closed \
             without it"
        ));
    }
    let run = run_payloads(&block)
        .into_iter()
        .find(|r| r.contains("--test sql_store_contract"))
        .ok_or_else(|| {
            format!(
                "`{INTEGRATION_JOB}` never runs the DSN-gated suites (no \
                 `--test sql_store_contract` in any run step)"
            )
        })?;
    if !run.contains("--ignored") {
        return Err(
            "the DSN-gated tests are #[ignore]d — without `--ignored` the \
             integration job runs nothing and reports green over zero tests"
                .to_owned(),
        );
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
fn ci_runs_the_guard_tests() {
    let path = repo_root().join(CI_WORKFLOW);
    let src = std::fs::read_to_string(&path).unwrap();
    if let Err(why) = why_guard_tests_not_wired(&src) {
        panic!("{CI_WORKFLOW} no longer wires up the guard tests: {why}");
    }
}

#[test]
fn required_check_names_are_exposed_and_documented() {
    let path = repo_root().join(CI_WORKFLOW);
    let src = std::fs::read_to_string(&path).unwrap();
    if let Err(why) = why_check_names_not_enactable(&src) {
        panic!(
            "{CI_WORKFLOW} does not make merge-blocking enactable from repo \
             state alone: {why}"
        );
    }
}

#[test]
fn ci_runs_the_dsn_gated_integration_tests() {
    let path = repo_root().join(CI_WORKFLOW);
    let src = std::fs::read_to_string(&path).unwrap();
    if let Err(why) = why_integration_tests_not_wired(&src) {
        panic!("{CI_WORKFLOW} no longer wires up the DSN-gated integration tests: {why}");
    }
}

#[test]
fn a_regated_job_is_caught() {
    let src = "\
on:
  push:
    branches: [main]
  workflow_dispatch:
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
  workflow_dispatch:
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
  workflow_dispatch:
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
  workflow_dispatch:
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
  workflow_dispatch:
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
  workflow_dispatch:
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
  workflow_dispatch:
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
  workflow_dispatch:
jobs:
  deploy-build:
    name: release build (linux, as the Docker builder sees it)
    steps:
      - run: cargo build --release --bin coxagent
";
    let why = why_quality_gates_not_wired(src).unwrap_err();
    assert!(why.contains("no `check` job"), "unhelpful: {why}");
}

/// A minimal workflow that satisfies every F286 + F326 rule — the positive
/// control each `..._is_caught` fixture below mutates.
fn wired_workflow() -> String {
    "\
# Merge-blocking from repo state alone: branch protection on main must
# require these status checks, by their check-run display names exactly:
#   - docker compose smoke (app answers on 8101)
#   - ownership + CI wiring guards
on:
  push:
    branches: [main]
  workflow_dispatch:
jobs:
  deploy-smoke:
    name: docker compose smoke (app answers on 8101)
    runs-on: ubuntu-latest
    timeout-minutes: 30
    steps:
      - run: cargo test -p coxagent-app --test deploy_smoke -- --ignored
      - name: logs
        if: failure()
        run: docker compose logs
      - name: tear down
        if: always()
        run: docker compose down -v
  guard-tests:
    name: ownership + CI wiring guards
    runs-on: ubuntu-latest
    steps:
      - run: cargo test -p coxagent-infrastructure --lib reclaimable
      - run: cargo test -p coxagent-app --test deploy_smoke --test ci_availability_gate --test test_env_guard_f326_gate
      - run: cargo test -p coxagent-presentation --test guardrail_scaffold_b201
  integration:
    name: integration (ephemeral cxa_test postgres + redis)
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:16-alpine
        env:
          POSTGRES_DB: cxa_test
        ports:
          - 5432:5432
      redis:
        image: redis:7-alpine
        ports:
          - 6379:6379
    env:
      COXAGENT_TEST_PG_DSN: postgres://cox:test@localhost:5432/cxa_test
      COXAGENT_TEST_REDIS_URL: redis://localhost:6379
    steps:
      - run: cargo test -p coxagent-infrastructure --test sql_store_contract --test kv_doc_contract --test sql_auth_token_harvest --test distributed_coord -- --ignored
"
    .to_owned()
}

#[test]
fn a_fully_wired_workflow_passes_every_f286_rule() {
    let src = wired_workflow();
    why_not_wired(&src).unwrap_or_else(|why| panic!("smoke half of the control: {why}"));
    why_guard_tests_not_wired(&src)
        .unwrap_or_else(|why| panic!("guard half of the control: {why}"));
    why_check_names_not_enactable(&src)
        .unwrap_or_else(|why| panic!("check-name half of the control: {why}"));
    why_integration_tests_not_wired(&src)
        .unwrap_or_else(|why| panic!("integration half of the control: {why}"));
}

#[test]
fn a_regated_guard_tests_job_is_caught() {
    let src = wired_workflow().replace(
        "  guard-tests:\n    name:",
        "  guard-tests:\n    if: false\n    name:",
    );
    let why = why_guard_tests_not_wired(&src).unwrap_err();
    assert!(why.contains("disabled by a job-level"), "unhelpful: {why}");
    assert!(why.contains("if: false"), "unhelpful: {why}");
}

#[test]
fn a_job_level_continue_on_error_is_caught_on_both_gate_jobs() {
    // A failed run that still reports green bypasses the required check
    // exactly like `if: false` — the guard must treat it as a disable.
    let smoke = wired_workflow().replace(
        "  deploy-smoke:\n    name:",
        "  deploy-smoke:\n    continue-on-error: true\n    name:",
    );
    let why = why_not_wired(&smoke).unwrap_err();
    assert!(why.contains("disabled by a job-level"), "unhelpful: {why}");
    assert!(why.contains("continue-on-error"), "unhelpful: {why}");

    let guard = wired_workflow().replace(
        "  guard-tests:\n    name:",
        "  guard-tests:\n    continue-on-error: true\n    name:",
    );
    let why = why_guard_tests_not_wired(&guard).unwrap_err();
    assert!(why.contains("disabled by a job-level"), "unhelpful: {why}");
    assert!(why.contains("continue-on-error"), "unhelpful: {why}");
}

#[test]
fn dropping_a_guard_command_is_caught() {
    let no_policy = wired_workflow().replace(
        "      - run: cargo test -p coxagent-infrastructure --lib reclaimable\n",
        "",
    );
    let why = why_guard_tests_not_wired(&no_policy).unwrap_err();
    assert!(why.contains("--lib reclaimable"), "unhelpful: {why}");

    let no_wiring = wired_workflow().replace(
        "      - run: cargo test -p coxagent-app --test deploy_smoke --test \
         ci_availability_gate --test test_env_guard_f326_gate\n",
        "",
    );
    let why = why_guard_tests_not_wired(&no_wiring).unwrap_err();
    assert!(
        why.contains("--test ci_availability_gate"),
        "unhelpful: {why}"
    );
}

#[test]
fn a_missing_guard_tests_job_is_caught() {
    let src = wired_workflow()
        .split("  guard-tests:")
        .next()
        .unwrap()
        .to_owned();
    let why = why_guard_tests_not_wired(&src).unwrap_err();
    assert!(why.contains("no `guard-tests` job"), "unhelpful: {why}");
}

#[test]
fn docker_in_the_guard_job_is_caught() {
    let src = wired_workflow().replace(
        "      - run: cargo test -p coxagent-infrastructure --lib reclaimable",
        "      - run: docker compose up -d && cargo test -p \
         coxagent-infrastructure --lib reclaimable",
    );
    let why = why_guard_tests_not_wired(&src).unwrap_err();
    assert!(why.contains("docker"), "unhelpful: {why}");
}

#[test]
fn step_level_conditions_in_the_guard_job_are_not_a_gate() {
    let src = wired_workflow().replace(
        "      - run: cargo test -p coxagent-infrastructure --lib reclaimable",
        "      - name: post-mortem\n        if: failure()\n        run: echo \
         fell over\n      - run: cargo test -p coxagent-infrastructure --lib \
         reclaimable",
    );
    why_guard_tests_not_wired(&src)
        .unwrap_or_else(|why| panic!("step conditions read as a gate: {why}"));
}

#[test]
fn a_drifted_or_undocumented_check_name_is_caught() {
    let renamed = wired_workflow().replace(
        "    name: ownership + CI wiring guards",
        "    name: guard checks",
    );
    let why = why_check_names_not_enactable(&renamed).unwrap_err();
    assert!(why.contains("drifted"), "unhelpful: {why}");

    let no_display_name = wired_workflow().replace("    name: ownership + CI wiring guards\n", "");
    let why = why_check_names_not_enactable(&no_display_name).unwrap_err();
    assert!(why.contains("declares no display"), "unhelpful: {why}");

    let undocumented = wired_workflow()
        .replace("#   - docker compose smoke (app answers on 8101)\n", "")
        .replace("#   - ownership + CI wiring guards\n", "");
    let why = why_check_names_not_enactable(&undocumented).unwrap_err();
    assert!(why.contains("no longer documents"), "unhelpful: {why}");

    let no_context = wired_workflow().replace(
        "# Merge-blocking from repo state alone: branch protection on main must",
        "# Merge-blocking from repo state alone: an operator must",
    );
    let why = why_check_names_not_enactable(&no_context).unwrap_err();
    assert!(why.contains("branch protection"), "unhelpful: {why}");
}

/// F326: the integration job missing, `if:`-gated, or `continue-on-error`ed
/// is caught — all three leave the gated suites unexecuted while CI reports
/// green.
#[test]
fn an_integration_job_missing_gated_or_silently_failing_is_caught() {
    let missing = wired_workflow()
        .split("  integration:")
        .next()
        .unwrap()
        .to_owned();
    let why = why_integration_tests_not_wired(&missing).unwrap_err();
    assert!(why.contains("no `integration` job"), "unhelpful: {why}");

    let gated = wired_workflow().replace(
        "  integration:\n    name:",
        "  integration:\n    if: false\n    name:",
    );
    let why = why_integration_tests_not_wired(&gated).unwrap_err();
    assert!(why.contains("disabled by a job-level"), "unhelpful: {why}");

    let coe = wired_workflow().replace(
        "  integration:\n    name:",
        "  integration:\n    continue-on-error: true\n    name:",
    );
    let why = why_integration_tests_not_wired(&coe).unwrap_err();
    assert!(why.contains("disabled by a job-level"), "unhelpful: {why}");
    assert!(why.contains("continue-on-error"), "unhelpful: {why}");
}

/// F326: dropping an ephemeral service container is caught — the gated tests
/// refuse to run against anything the job does not own.
#[test]
fn dropping_a_service_container_is_caught() {
    let no_postgres = wired_workflow().replace("        image: postgres:16-alpine\n", "");
    let why = why_integration_tests_not_wired(&no_postgres).unwrap_err();
    assert!(why.contains("image: postgres"), "unhelpful: {why}");

    let no_redis = wired_workflow().replace("        image: redis:7-alpine\n", "");
    let why = why_integration_tests_not_wired(&no_redis).unwrap_err();
    assert!(why.contains("image: redis"), "unhelpful: {why}");
}

/// F326: dropping the DSN export, or renaming the database off `cxa_test*`,
/// is caught — the guard fails closed on the first and refuses the second.
#[test]
fn dropping_or_renaming_the_test_dsn_is_caught() {
    let dropped = wired_workflow().replace(
        "      COXAGENT_TEST_PG_DSN: postgres://cox:test@localhost:5432/cxa_test\n",
        "",
    );
    let why = why_integration_tests_not_wired(&dropped).unwrap_err();
    assert!(why.contains("no longer exports"), "unhelpful: {why}");

    let renamed = wired_workflow().replace("localhost:5432/cxa_test", "localhost:5432/coxagent");
    let why = why_integration_tests_not_wired(&renamed).unwrap_err();
    assert!(why.contains("cxa_test"), "unhelpful: {why}");

    let no_redis = wired_workflow().replace(
        "      COXAGENT_TEST_REDIS_URL: redis://localhost:6379\n",
        "",
    );
    let why = why_integration_tests_not_wired(&no_redis).unwrap_err();
    assert!(why.contains("COXAGENT_TEST_REDIS_URL"), "unhelpful: {why}");
}

/// F326: dropping `--ignored` from the integration run step is caught — the
/// gated tests are `#[ignore]`d, so the job would report green over zero
/// executed tests.
#[test]
fn dropping_ignored_from_the_integration_run_is_caught() {
    let dropped = wired_workflow().replace(" -- --ignored", "");
    let why = why_integration_tests_not_wired(&dropped).unwrap_err();
    assert!(why.contains("--ignored"), "unhelpful: {why}");
}

/// Step-level `if: failure()` / `if: always()` in the integration job are
/// legitimate (log collection, teardown) and must not read as a job gate.
#[test]
fn step_level_conditions_in_the_integration_job_are_not_a_gate() {
    let src = wired_workflow().replace(
        "      - run: cargo test -p coxagent-infrastructure --test sql_store_contract",
        "      - name: logs\n        if: failure()\n        run: echo fell over\n      \
         - run: cargo test -p coxagent-infrastructure --test sql_store_contract",
    );
    why_integration_tests_not_wired(&src)
        .unwrap_or_else(|why| panic!("step conditions read as a gate: {why}"));
}
