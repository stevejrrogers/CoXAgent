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
//! F326 added the last pin: an `integration` job must run the DB-backed
//! Postgres/Redis suites so CI proves them against the ephemeral shape they
//! always get. F327 (merged) changed HOW they are provisioned, so the pin
//! moved with it: every test claims its own ephemeral postgres+redis through
//! the shared compose fixture in `infrastructure/tests/common/mod.rs`, which
//! REFUSES any exported `COXAGENT_TEST_PG_DSN` / `COXAGENT_TEST_REDIS_URL`
//! and nothing is `#[ignore]`d any more. The job therefore must exist, be
//! ungated, run all four suites — and must NOT export the refused env vars
//! (every claim would panic), carry its own `services:` containers (the
//! fixture is the one provisioner) nor pass `--ignored` (cargo would run
//! nothing and the job would paint green over zero executed tests).
//!
//! F340 turns the lesson of that sync into a rule — policy and pins move
//! together. The cross-check section below reads the shared guard's source
//! and the guard gate's `GATED` roster and fails the build when one side of
//! the contract moves without the other: a suite in the job that no gate
//! covers (or a gated suite the job never runs), an env var the guard refuses
//! that the job exports (or a refusal the pins no longer know), a fixture
//! database renamed off the guard's DSN path, or the rule's statement
//! dropped from the integration job's comment or the fixture header. Every
//! unreadable input fails closed.

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
const GUARD_TESTS_STEPS: [&str; 2] = [
    "cargo test -p coxagent-infrastructure --lib reclaimable",
    "cargo test -p coxagent-app --test deploy_smoke --test ci_availability_gate \
     --test test_env_guard_f326_gate",
];

/// The job that runs the DB-backed integration suites (F326 job, F327
/// policy) — the CI-level half of the fail-closed test-database policy. The
/// compose fixture provisions each test's own ephemeral pair, so the job
/// must not export a database env var, carry its own service containers, nor
/// filter with `--ignored`.
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

/// The four DB-backed integration suites the `integration` job must run.
const INTEGRATION_SUITES: [&str; 4] = [
    "--test sql_store_contract",
    "--test kv_doc_contract",
    "--test sql_auth_token_harvest",
    "--test distributed_coord",
];

/// The env vars the compose fixture REFUSES (F327): an exported test-database
/// URL makes every claim panic red, so exporting one in the job is a wiring
/// bug the gate must catch.
const REFUSED_ENV_VARS: [&str; 2] = ["COXAGENT_TEST_PG_DSN", "COXAGENT_TEST_REDIS_URL"];

/// Why would CI never run the DB-backed integration tests (F326 job, F327
/// policy)? `Ok` only when the `integration` job exists, is ungated, and runs
/// all four DB-backed suites — which the compose fixture provisions by
/// itself. It must NOT export `COXAGENT_TEST_PG_DSN` /
/// `COXAGENT_TEST_REDIS_URL` (the fixture refuses any exported URL, so the
/// job would run nothing but refusals), must NOT carry its own `services:`
/// containers (the fixture is the one provisioner; a job-owned database is
/// the shared target the policy retires), and must NOT pass `--ignored`
/// (nothing is `#[ignore]`d; the flag would execute zero tests and report
/// green over them).
fn why_integration_tests_not_wired(src: &str) -> Result<(), String> {
    let block = job_block(src, INTEGRATION_JOB).ok_or_else(|| {
        format!(
            "no `{INTEGRATION_JOB}` job in the workflow — the DB-backed \
             integration suites would never run"
        )
    })?;
    if let Some(gate) = job_gate(&block) {
        return Err(format!(
            "`{INTEGRATION_JOB}` is disabled by a job-level `{gate}` — the \
             integration tests never run"
        ));
    }
    for var in REFUSED_ENV_VARS {
        if env_value(&block, var).is_some() {
            return Err(format!(
                "`{INTEGRATION_JOB}` exports {var} — the shared compose \
                 fixture refuses any exported test-database URL (CXA-F327 \
                 fail-closed policy), so every claim would panic and the job \
                 would run nothing but refusals"
            ));
        }
    }
    if block.lines().any(|l| l.starts_with("    services:")) {
        return Err(format!(
            "`{INTEGRATION_JOB}` carries its own `services:` containers — the \
             shared compose fixture (CXA-F327) provisions each test's own \
             ephemeral pair, so a job-owned database is the shared target the \
             policy exists to retire"
        ));
    }
    let runs = run_payloads(&block);
    for suite in INTEGRATION_SUITES {
        if !runs.iter().any(|r| r.contains(suite)) {
            return Err(format!(
                "`{INTEGRATION_JOB}` no longer runs `{suite}` — the job is the \
                 CI record that all four DB-backed suites pass"
            ));
        }
    }
    if let Some(run) = runs.iter().find(|r| r.contains("--ignored")) {
        return Err(format!(
            "nothing is #[ignore]d any more — `--ignored` in `{run}` executes \
             zero tests and reports green over them"
        ));
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// --- F340: the CI pins and the shared guard's contract move together ---
//
// The F327 sync proved the failure mode: the landed guard refused exported
// DSNs while the integration job still exported one and both pins defended
// the dead F326 contract — CI stayed green over zero executed suites until a
// follow-up PR moved the pins. This section cross-checks the two sides so a
// one-sided move fails the build here, with a message that says to land both
// in the same PR. Everything below is a pure decision over file text — no
// server, port or docker.

/// The gate file whose `GATED` roster the integration job's suite list must
/// mirror. Its binary is itself pinned in the guard-tests command
/// (`GUARD_TESTS_STEPS`), so dropping this cross-check from CI is caught by
/// [`why_guard_tests_not_wired`] — self-wiring.
const GUARD_GATE_FILE: &str = "crates/app/tests/test_env_guard_f326_gate.rs";

/// The shared fail-closed guard whose observable env contract the workflow
/// must agree with.
const SHARED_GUARD_FILE: &str = "crates/infrastructure/tests/common/mod.rs";

/// The compose fixture the guard claims per test; its `POSTGRES_DB` must be
/// the database the guard's DSN path names.
const TEST_PG_COMPOSE_FILE: &str = "crates/infrastructure/tests/common/test-pg.compose.yml";

/// The rule, stated where the next engineer would hit it: the integration
/// job's comment in ci.yml and the shared fixture's header. The gate fails
/// the build if either statement disappears.
const TOGETHER_RULE: &str = "Policy and pins move together";

/// A repo file's text, or a named error — the cross-check fails closed when
/// a file it needs is missing, never green over an unreadable contract.
fn read_repo(rel: &str) -> Result<String, String> {
    std::fs::read_to_string(repo_root().join(rel)).map_err(|e| format!("cannot read {rel}: {e}"))
}

/// The suite roster the `integration` job runs: every `--test <name>` token
/// in its run payloads. `Err` when the job runs nothing parseable — the
/// cross-check fails closed instead of passing over an unreadable workflow.
fn integration_roster(src: &str) -> Result<Vec<String>, String> {
    let block = job_block(src, INTEGRATION_JOB)
        .ok_or_else(|| format!("no `{INTEGRATION_JOB}` job in the workflow"))?;
    let mut roster = Vec::new();
    for run in run_payloads(&block) {
        let mut tokens = run.split_whitespace();
        while let Some(token) = tokens.next() {
            if token == "--test" {
                let suite = tokens.next().ok_or_else(|| {
                    format!(
                        "`--test` with no suite name in the `{INTEGRATION_JOB}` \
                         job's run (`{run}`) — the roster is unparsable, \
                         failing closed"
                    )
                })?;
                roster.push(suite.to_owned());
            }
        }
    }
    if roster.is_empty() {
        return Err(format!(
            "the `{INTEGRATION_JOB}` job runs no `--test <suite>` target — the \
             roster is unparsable, failing closed"
        ));
    }
    Ok(roster)
}

/// The gated-suite roster pinned by the guard gate's `const GATED:` marker,
/// parsed from its source so the two files cannot drift silently. `Err` when
/// the marker is absent, unclosed, or yields no `*.rs` entries — an
/// unparsable roster fails closed rather than passing vacuously.
fn gated_roster_from(gate_src: &str) -> Result<Vec<String>, String> {
    const MARKER: &str = "const GATED:";
    let at = gate_src.find(MARKER).ok_or_else(|| {
        format!(
            "{GUARD_GATE_FILE} no longer carries the `{MARKER}` roster marker — \
             the cross-check cannot verify the CI roster against it; update \
             both sides in the same PR"
        )
    })?;
    let body = &gate_src[at..];
    let end = body.find("];").ok_or_else(|| {
        format!(
            "the `{MARKER}` roster in {GUARD_GATE_FILE} is never closed with \
             `];` — unparsable, failing closed"
        )
    })?;
    let mut roster = Vec::new();
    for entry in body[..end].split('"').skip(1).step_by(2) {
        let stem = entry.strip_suffix(".rs").ok_or_else(|| {
            format!(
                "GATED entry `{entry}` does not name a test file (`*.rs`) — the \
                 roster marker in {GUARD_GATE_FILE} is unparsable, failing closed"
            )
        })?;
        roster.push(stem.to_owned());
    }
    if roster.is_empty() {
        return Err(format!(
            "the `{MARKER}` roster in {GUARD_GATE_FILE} parsed to zero suites — \
             unparsable, failing closed"
        ));
    }
    Ok(roster)
}

/// The env vars the shared guard refuses, read from its source (every
/// `const EXPORTED_*_VAR: &str = "..."` declaration) so the workflow is
/// checked against the guard's ACTUAL contract, not a second hand-copied
/// list. `Err` when the contract is unreadable — the cross-check fails
/// closed.
fn guard_refused_env_vars_from(guard_src: &str) -> Result<Vec<String>, String> {
    let mut refused = Vec::new();
    for line in guard_src.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("const EXPORTED_") {
            continue;
        }
        let name = trimmed
            .split(": &str = \"")
            .nth(1)
            .and_then(|value| value.split('"').next())
            .unwrap_or_default();
        if name.is_empty() {
            return Err(format!(
                "the shared guard declares an `EXPORTED_` var with no readable \
                 name in {SHARED_GUARD_FILE} — its env contract is unparsable, \
                 failing closed"
            ));
        }
        refused.push(name.to_owned());
    }
    if refused.is_empty() {
        return Err(format!(
            "the shared guard in {SHARED_GUARD_FILE} no longer declares any \
             `const EXPORTED_*_VAR` — its observable env contract moved; \
             update this cross-check in the same PR"
        ));
    }
    Ok(refused)
}

/// The roster half of the cross-check, both directions with the same-PR
/// message: every suite the job runs must be gated, every gated suite must
/// be run.
fn roster_disagreements(ci_roster: &[String], gated: &[String]) -> Result<(), String> {
    for suite in ci_roster {
        if !gated.contains(suite) {
            return Err(format!(
                "`{suite}` runs in the `{INTEGRATION_JOB}` job but is not in \
                 {GUARD_GATE_FILE}'s GATED roster — the CI pins and the shared \
                 guard move together in the same PR (add it to GATED or drop \
                 it from the job)"
            ));
        }
    }
    for suite in gated {
        if !ci_roster.contains(suite) {
            return Err(format!(
                "`{suite}.rs` is in {GUARD_GATE_FILE}'s GATED roster but the \
                 `{INTEGRATION_JOB}` job never runs it — the CI pins and the \
                 shared guard move together in the same PR (add \
                 `--test {suite}` to the job or drop it from GATED)"
            ));
        }
    }
    Ok(())
}

/// The env-var half of the cross-check: the job must export none of the vars
/// the guard refuses, and the pins' own [`REFUSED_ENV_VARS`] must stay a
/// subset of the guard's actual refused set — either side moving alone is
/// caught.
fn env_contract_disagreements(block: &str, refused: &[String]) -> Result<(), String> {
    for var in refused {
        if env_value(block, var).is_some() {
            return Err(format!(
                "`{INTEGRATION_JOB}` exports {var} but the shared guard refuses \
                 it — the guard's env contract and the workflow move together \
                 in the same PR (unset it in the job, or update the pins)"
            ));
        }
    }
    for var in REFUSED_ENV_VARS {
        if !refused.iter().any(|r| r == var) {
            return Err(format!(
                "the shared guard no longer refuses `{var}` by name — its env \
                 contract and the pins in this file move together in the same \
                 PR (update REFUSED_ENV_VARS and the workflow together)"
            ));
        }
    }
    Ok(())
}

/// The fixture's `POSTGRES_DB` must be the database the guard's DSN path
/// names — renaming one without the other passes every gate here and then
/// fails every claim RED at schema-init, after CI already burned minutes.
fn fixture_db_matches_guard_dsn_path(compose_src: &str, guard_src: &str) -> Result<(), String> {
    /// The guard's DSN format up to the database name it appends.
    const DSN_PATH: &str = "@127.0.0.1:{pg_port}/";
    let db = compose_src
        .lines()
        .find_map(|l| l.trim().strip_prefix("POSTGRES_DB:"))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            format!(
                "{TEST_PG_COMPOSE_FILE} no longer declares `POSTGRES_DB:` — the \
                 fixture and the guard's DSN path move together in the same PR"
            )
        })?;
    let dsn_db = guard_src
        .find(DSN_PATH)
        .and_then(|at| guard_src[at + DSN_PATH.len()..].split('"').next())
        .filter(|d| !d.is_empty())
        .ok_or_else(|| {
            format!(
                "{SHARED_GUARD_FILE} no longer builds a DSN with a readable \
                 database path (`{DSN_PATH}<db>`) — the fixture and the \
                 guard's DSN path move together in the same PR"
            )
        })?;
    if dsn_db != db {
        return Err(format!(
            "the fixture provisions database `{db}` but the guard's DSN path \
             names `{dsn_db}` — renaming one without the other fails every \
             claim RED at schema-init; the fixture and the guard move \
             together in the same PR"
        ));
    }
    Ok(())
}

/// The rule statement (AC): the phrase must live in the integration job's
/// comment in ci.yml and in the shared fixture's header — the two places the
/// next engineer edits one side without the other.
fn the_rule_is_stated_in_both_places(ci_comments: &str, guard_src: &str) -> Result<(), String> {
    if !ci_comments.contains(TOGETHER_RULE) {
        return Err(format!(
            "the `{INTEGRATION_JOB}` job's comment in {CI_WORKFLOW} no longer \
             states the rule ({TOGETHER_RULE}) — restore it together with any \
             contract change; the rule is what keeps the next sync from being \
             a follow-up PR"
        ));
    }
    if !guard_src.contains(TOGETHER_RULE) {
        return Err(format!(
            "the shared fixture header ({SHARED_GUARD_FILE}) no longer states \
             the rule ({TOGETHER_RULE}) — restore it together with any guard \
             change"
        ));
    }
    Ok(())
}

/// The whole F340 cross-check over the real repo files. `Ok` only when the
/// rosters agree both ways, the env contracts agree both ways, the fixture's
/// database matches the guard's DSN path — and every unreadable input fails
/// closed.
fn why_ci_pins_and_guard_contract_disagree(ci_src: &str) -> Result<(), String> {
    let ci_roster = integration_roster(ci_src)?;
    let gated = gated_roster_from(&read_repo(GUARD_GATE_FILE)?)?;
    roster_disagreements(&ci_roster, &gated)?;
    let block = job_block(ci_src, INTEGRATION_JOB)
        .ok_or_else(|| format!("no `{INTEGRATION_JOB}` job in the workflow"))?;
    let refused = guard_refused_env_vars_from(&read_repo(SHARED_GUARD_FILE)?)?;
    env_contract_disagreements(&block, &refused)?;
    fixture_db_matches_guard_dsn_path(
        &read_repo(TEST_PG_COMPOSE_FILE)?,
        &read_repo(SHARED_GUARD_FILE)?,
    )
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
        panic!("{CI_WORKFLOW} no longer wires up the DB-backed integration suites: {why}");
    }
}

/// F340 AC: the guard-tests job's cross-check — the CI pins and the shared
/// guard's observable env contract must agree, or the build fails naming the
/// side that moved alone.
#[test]
fn the_ci_pins_and_the_guard_contract_agree() {
    let src = std::fs::read_to_string(repo_root().join(CI_WORKFLOW)).unwrap();
    if let Err(why) = why_ci_pins_and_guard_contract_disagree(&src) {
        panic!("the CI pins and the shared guard's contract disagree: {why}");
    }
}

/// F340 AC: the rule is stated where the next engineer would hit it — the
/// integration job's comment in ci.yml and the shared fixture header.
#[test]
fn the_together_rule_is_stated_where_engineers_hit_it() {
    let ci = std::fs::read_to_string(repo_root().join(CI_WORKFLOW)).unwrap();
    let guard = std::fs::read_to_string(repo_root().join(SHARED_GUARD_FILE)).unwrap();
    if let Err(why) = the_rule_is_stated_in_both_places(&comment_text(&ci), &guard) {
        panic!("the policy-and-pins-together rule lost its statement: {why}");
    }
}

#[test]
fn dropping_the_rule_statement_from_either_side_is_caught() {
    let no_ci = the_rule_is_stated_in_both_places("# an unrelated comment\n", TOGETHER_RULE);
    assert!(no_ci.is_err(), "a ci.yml without the rule statement must fail");

    let no_guard = the_rule_is_stated_in_both_places(TOGETHER_RULE, "//! guard docs\n");
    assert!(
        no_guard.is_err(),
        "a shared guard header without the rule statement must fail"
    );
}

/// Roster mutants, both directions with the same-PR message.
#[test]
fn a_roster_move_on_one_side_alone_is_caught() {
    let extra_in_ci = wired_workflow().replace(
        "--test distributed_coord",
        "--test distributed_coord --test kv_doc_contract_extra",
    );
    let ci_roster = integration_roster(&extra_in_ci).unwrap();
    let gated = gated_roster_from(&std::fs::read_to_string(repo_root().join(GUARD_GATE_FILE)).unwrap())
        .unwrap();
    let why = roster_disagreements(&ci_roster, &gated).unwrap_err();
    assert!(why.contains("kv_doc_contract_extra"), "unhelpful: {why}");
    assert!(why.contains("same PR"), "unhelpful: {why}");

    let missing_from_ci = wired_workflow().replace(" --test distributed_coord", "");
    let ci_roster = integration_roster(&missing_from_ci).unwrap();
    let why = roster_disagreements(&ci_roster, &gated).unwrap_err();
    assert!(why.contains("distributed_coord"), "unhelpful: {why}");
    assert!(why.contains("same PR"), "unhelpful: {why}");
}

/// An unreadable roster marker fails closed — never a green over an
/// unverifiable agreement.
#[test]
fn an_unparsable_gated_roster_fails_closed() {
    let no_marker = "const OTHER: &[&str] = &[\"x.rs\"];\n";
    let why = gated_roster_from(no_marker).unwrap_err();
    assert!(why.contains("const GATED:"), "unhelpful: {why}");

    let unclosed = "const GATED: &[&str] = &[\n    \"sql_store_contract.rs\",\n";
    let why = gated_roster_from(unclosed).unwrap_err();
    assert!(why.contains("];"), "unhelpful: {why}");

    let not_rs = "const GATED: &[&str] = &[\"sql_store_contract\"];\n";
    let why = gated_roster_from(not_rs).unwrap_err();
    assert!(why.contains("*.rs"), "unhelpful: {why}");

    let empty = "const GATED: &[&str] = &[];\n";
    let why = gated_roster_from(empty).unwrap_err();
    assert!(why.contains("zero suites"), "unhelpful: {why}");

    let unparsable_run = "\
on:
  push:
    branches: [main]
jobs:
  integration:
    name: integration
    steps:
      - run: cargo test --test
";
    let why = integration_roster(unparsable_run).unwrap_err();
    assert!(why.contains("no suite name"), "unhelpful: {why}");

    let no_runs = "\
on:
  push:
    branches: [main]
jobs:
  integration:
    name: integration
    steps:
      - uses: actions/checkout@v4
";
    let why = integration_roster(no_runs).unwrap_err();
    assert!(why.contains("unparsable, failing closed"), "unhelpful: {why}");
}

/// Env-contract mutants: the guard starts refusing a variable the job still
/// exports, the guard drops a refusal the pins still know, and the guard's
/// contract becomes unreadable — each is caught, naming the same-PR rule.
#[test]
fn an_env_contract_move_on_one_side_alone_is_caught() {
    let block = "  integration:\n    env:\n      COXAGENT_TEST_PG_HOST: localhost\n";
    let refused = vec![
        "COXAGENT_TEST_PG_DSN".to_owned(),
        "COXAGENT_TEST_REDIS_URL".to_owned(),
        "COXAGENT_TEST_PG_HOST".to_owned(),
    ];
    let why = env_contract_disagreements(block, &refused).unwrap_err();
    assert!(why.contains("COXAGENT_TEST_PG_HOST"), "unhelpful: {why}");
    assert!(why.contains("same PR"), "unhelpful: {why}");

    let dropped = vec!["COXAGENT_TEST_PG_DSN".to_owned()];
    let why = env_contract_disagreements("  integration:\n", &dropped).unwrap_err();
    assert!(
        why.contains("no longer refuses `COXAGENT_TEST_REDIS_URL`"),
        "unhelpful: {why}"
    );

    let why = guard_refused_env_vars_from("fn claim() {}").unwrap_err();
    assert!(why.contains("EXPORTED_"), "unhelpful: {why}");

    let unnamed = "const EXPORTED_DSN_VAR: &str = \"\";\n";
    let why = guard_refused_env_vars_from(unnamed).unwrap_err();
    assert!(why.contains("no readable name"), "unhelpful: {why}");
}

/// The fixture's database drifting off the guard's DSN path is caught here —
/// before CI spends minutes on docker only to fail every claim at schema-init.
#[test]
fn renaming_the_fixture_database_off_the_guard_dsn_path_is_caught() {
    let guard = "\
let dsn = format!(\"postgres://postgres:{pg_password}@127.0.0.1:{pg_port}/coxagent\");
";
    let renamed = fixture_db_matches_guard_dsn_path("POSTGRES_DB: other\n", guard);
    let why = renamed.unwrap_err();
    assert!(why.contains("`other`"), "unhelpful: {why}");
    assert!(why.contains("`coxagent`"), "unhelpful: {why}");

    let no_db = fixture_db_matches_guard_dsn_path("services:\n", guard);
    assert!(no_db.is_err(), "an undeclared POSTGRES_DB must fail closed");

    let no_dsn_path = fixture_db_matches_guard_dsn_path("POSTGRES_DB: coxagent\n", "fn claim() {}\n");
    assert!(no_dsn_path.is_err(), "an unreadable DSN path must fail closed");
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
  integration:
    name: integration (ephemeral postgres + redis via compose fixture)
    runs-on: ubuntu-latest
    steps:
      - run: cargo test -p coxagent-infrastructure --test sql_store_contract --test kv_doc_contract --test sql_auth_token_harvest --test distributed_coord
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

/// F327: exporting either refused database env var in the integration job is
/// caught — the compose fixture refuses any exported test-database URL, so
/// the job would run nothing but refusals while appearing wired.
#[test]
fn an_exported_database_env_var_in_the_integration_job_is_caught() {
    let with_dsn = wired_workflow().replace(
        "    steps:\n      - run: cargo test -p coxagent-infrastructure --test \
         sql_store_contract",
        "    env:\n      COXAGENT_TEST_PG_DSN: \
         postgres://cox:test@localhost:5432/cxa_test\n    steps:\n      - run: \
         cargo test -p coxagent-infrastructure --test sql_store_contract",
    );
    let why = why_integration_tests_not_wired(&with_dsn).unwrap_err();
    assert!(why.contains("COXAGENT_TEST_PG_DSN"), "unhelpful: {why}");

    let with_redis = wired_workflow().replace(
        "    steps:\n      - run: cargo test -p coxagent-infrastructure --test \
         sql_store_contract",
        "    env:\n      COXAGENT_TEST_REDIS_URL: redis://localhost:6379\n    \
         steps:\n      - run: cargo test -p coxagent-infrastructure --test \
         sql_store_contract",
    );
    let why = why_integration_tests_not_wired(&with_redis).unwrap_err();
    assert!(why.contains("COXAGENT_TEST_REDIS_URL"), "unhelpful: {why}");
}

/// F327: re-adding a job-owned `services:` block to the integration job is
/// caught — the compose fixture is the one provisioner; a job-owned database
/// is the shared target the policy retires.
#[test]
fn a_job_owned_services_block_in_the_integration_job_is_caught() {
    let with_services = wired_workflow().replace(
        "  integration:\n    name:",
        "  integration:\n    services:\n      postgres:\n        image: \
         postgres:16-alpine\n    name:",
    );
    let why = why_integration_tests_not_wired(&with_services).unwrap_err();
    assert!(why.contains("services:"), "unhelpful: {why}");
}

/// F327: dropping one of the four suite targets from the integration run is
/// caught — the job is the CI record that ALL of them pass.
#[test]
fn dropping_a_suite_target_from_the_integration_run_is_caught() {
    let dropped = wired_workflow().replace(" --test distributed_coord", "");
    let why = why_integration_tests_not_wired(&dropped).unwrap_err();
    assert!(why.contains("--test distributed_coord"), "unhelpful: {why}");
}

/// F326/F327: passing `--ignored` to the integration run is caught — nothing
/// is `#[ignore]`d any more, so the flag would execute zero tests and report
/// green over them.
#[test]
fn passing_ignored_to_the_integration_run_is_caught() {
    let with_flag = wired_workflow().replace(
        "--test distributed_coord\n",
        "--test distributed_coord -- --ignored\n",
    );
    let why = why_integration_tests_not_wired(&with_flag).unwrap_err();
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
