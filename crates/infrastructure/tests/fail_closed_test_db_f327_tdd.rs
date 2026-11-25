//! CXA-F327 — Fail-closed test DB policy with shared compose fixture. RED
//! half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "Running the Postgres-backed integration tests via the shared compose
//!    fixture provisions a dedicated test database automatically — no
//!    manually exported COXAGENT_TEST_PG_DSN needed — and the full
//!    sql_store_contract suite executes (not skips) and passes green."
//! 2. "If the test database cannot be provisioned or reached, the DB-backed
//!    tests fail red with an explicit error naming the fixture/DSN; the
//!    current silent no-op path (missing DSN => test returns Ok) is gone."
//! 3. "If the configured DSN points at a live hub database (existing cxa row
//!    with revision > 0), every DB-backed suite (contract and auth harvest)
//!    aborts loudly and writes nothing — row count in that database is
//!    unchanged before/after the refusal, which surfaces as a failure, not a
//!    green skip."
//! 4. "In an environment without Docker, the DB-backed tests report an
//!    explicit skip with the reason visible in the run summary; with Docker
//!    available but the fixture stack failing to come up, the suite is red —
//!    a needed-but-absent database is never silently green."
//! 5. "Repeated and concurrent runs against the same fixture database are
//!    isolated (per-run project/schema), so back-to-back runs on one shared
//!    fixture neither fail on leftover state nor contaminate each other's
//!    rows."
//!
//! HOW THESE CRITERIA ARE ENCODED: pure source-scan guards over the real
//! suite sources and the real fixture module, plus a pure decision table and
//! scanner bite-controls over synthetic text — the same no-harness discipline
//! as `approval_policy_f303_tdd.rs`, `ci_availability_gate.rs` and
//! `live_repro_link_f247_tdd.rs`: no fake HTTP server, no host harness, no
//! network port, no invented identifiers. Every fixture input below is data
//! the codebase actually has today (the four DB-backed suites, the
//! `COXAGENT_TEST_PG_DSN` var, the `is_live_hub_db` probe over
//! `SqlStateStore::connect(.., "cxa")` + `current_version() > 0`, the repo's
//! compose conventions). A test that called the fixture directly could not
//! compile today (no fixture symbol exists anywhere in the workspace —
//! verified before writing this file), so the red half pins the missing
//! fixture where it must be declared, and the controls pin the semantics it
//! must implement. Every failing assertion below fails only because
//! CXA-F327's behaviour is missing; if an assertion's mechanism moves during
//! implementation, move the guard with it (the `preflight_f239_tdd.rs`
//! convention).
//!
//! Where the missing pieces must live: the fixture driver goes in
//! `crates/infrastructure/tests/common/mod.rs` — it is ALREADY the shared
//! module of the DSN-gated suites (`mod common;` in sql_auth_token_harvest
//! and distributed_coord), so every suite reaches it with zero new wiring;
//! the compose stack goes beside it as
//! `crates/infrastructure/tests/common/test-pg.compose.yml` — inside the
//! test tree, where it cannot be mistaken for a deployable stack and is
//! invisible to the product compose gates (`compose_security_gate.rs`,
//! `compose_healthcheck_gate.rs`, `docs_ports.rs`), which scan the root and
//! deploy/ stacks by path. The root `docker-compose.yml` is the PRODUCT
//! stack and `deploy/local-infra/docker-compose.yml` is LIVE dev
//! infrastructure (external `cox_pgdata` volume, host port 5432) — pointing
//! tests at either is exactly the live-hub incident AC3 forbids.
//!
//! Red today, and why (all line numbers verified before writing this file):
//!   * AC1 — no compose fixture exists anywhere in the repo; `common/mod.rs`
//!     (17 lines) never mentions docker, a compose file or provisioning; and
//!     `sql_store_contract.rs` does not even `mod common;` — each of its
//!     three tests gates itself on a manually exported DSN.
//!   * AC2 — the silent no-op path is live in all four DB-backed suites:
//!     `let Ok(dsn) = std::env::var("COXAGENT_TEST_PG_DSN") else {
//!     eprintln!(..); return; }` (sql_store_contract.rs:58-61, 118-121,
//!     193-196; sql_auth_token_harvest.rs:18-21; kv_doc_contract.rs:89-92,
//!     172-175) and the combined env check in distributed_coord.rs:64-67,
//!     141-144. A missing DSN returns Ok — green.
//!   * AC3 — every live-hub refusal that exists is a green skip:
//!     `if is_live_hub_db(..) { eprintln!(..); return; }`
//!     (sql_store_contract.rs:62-67, 122-125, 197-200;
//!     sql_auth_token_harvest.rs:22-27; distributed_coord.rs:68-73,
//!     145-150) — and `kv_doc_contract.rs` has NO live-hub refusal at all,
//!     so "every DB-backed suite" aborting loudly is impossible today.
//!   * AC4 — `common/mod.rs` contains no docker detection and no explicit
//!     skip branch; today a docker-less machine and a dockerful machine with
//!     a broken stack are indistinguishable: both green.
//!   * AC5 — the suites derive per-run ids from `std::process::id()` /
//!     nanotime (green half), but with a PERSISTENT shared fixture database
//!     nothing namespaces runs: concurrent `CREATE TABLE IF NOT EXISTS`
//!     migration races across processes are documented in the suites' own
//!     `MIGRATED` comments, and `test-<pid>` rows accumulate forever. The
//!     per-run schema/project namespacing the AC demands does not exist.
//!
//! AC → test map:
//! - AC1: [`ac1_the_shared_compose_fixture_exists_and_provisions_a_dedicated_test_database`]
//!   (RED), [`ac1_the_contract_suite_executes_through_the_shared_fixture_without_a_manual_dsn`]
//!   (RED)
//! - AC2: [`ac2_a_missing_or_unreachable_test_database_fails_red_naming_the_fixture_and_dsn`]
//!   (RED)
//! - AC3: [`ac3_a_live_hub_dsn_is_refused_loudly_by_every_db_backed_suite`]
//!   (RED)
//! - AC4: [`ac4_without_docker_the_db_backed_tests_report_an_explicit_skip`]
//!   (RED), [`ac4_with_docker_but_a_failed_fixture_stack_the_suite_is_red`]
//!   (RED)
//! - AC5: [`ac5_runs_are_isolated_per_run_and_never_contaminate_each_other`]
//!   (RED on the fixture half, green control on the suite-id half)
//! - Controls (green today, they pin the required semantics so the fixture
//!   implements THIS table and the scanners stay sharp):
//!   [`the_fail_closed_policy_table_pins_a_verdict_for_every_fixture_situation`],
//!   [`the_source_scanners_catch_the_silent_patterns_they_forbid`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// The shared fixture driver's home: already `mod common;` for
/// sql_auth_token_harvest and distributed_coord; the other two suites must
/// join it (AC1/AC2/AC3 all route through one policy).
const FIXTURE_MODULE: &str = "crates/infrastructure/tests/common/mod.rs";

/// The shared compose fixture stack: a dedicated, ephemeral Postgres beside
/// the driver that runs it. Pinned per the `approval_policy_f303_tdd.rs`
/// POLICY_MODULE convention — the red half names the declaration site.
const FIXTURE_COMPOSE: &str = "crates/infrastructure/tests/common/test-pg.compose.yml";

/// The DSN var the ACs name. It stays supported as an explicit override
/// (AC3's "the configured DSN") — the fixture's job is to make it OPTIONAL,
/// not to remove it.
const DSN_VAR: &str = "COXAGENT_TEST_PG_DSN";

/// Every suite whose tests need the test database — "every DB-backed suite"
/// in AC2/AC3 is exactly this set (contract, auth harvest, KV-doc contract,
/// distributed coordination).
const DB_SUITES: [&str; 4] = [
    "crates/infrastructure/tests/sql_store_contract.rs",
    "crates/infrastructure/tests/sql_auth_token_harvest.rs",
    "crates/infrastructure/tests/kv_doc_contract.rs",
    "crates/infrastructure/tests/distributed_coord.rs",
];

/// The live-hub probe every suite must route its refusal through (or
/// reproduce loudly): `common::is_live_hub_db` — the existing `cxa`-row /
/// `revision > 0` detector that closed the production-store incident.
const HUB_PROBE: &str = "is_live_hub_db";

/// How far past a needle a window may reach. A silent `return;` sits within
/// a few flattened characters of its needle; a cap keeps an UNRELATED later
/// return (e.g. another test's early exit) from being attributed to this
/// mention.
const WINDOW: usize = 600;

// --- repo-state scan helpers (the live_repro_link_f247_tdd.rs pattern) ------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The source with ALL whitespace removed, case preserved — guards match
/// exact source tokens in any formatting.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Successive windows of flattened source: from each occurrence of `needle`
/// to just before the next occurrence (or [`WINDOW`] chars later, whichever
/// is nearer). Windows are proximity heuristics over flat text, not parses.
fn windows<'a>(src: &'a str, needle: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(at) = rest.find(needle) {
        let after = &rest[at..];
        let next = after[needle.len()..]
            .find(needle)
            .map(|rel| needle.len() + rel);
        let end = next.unwrap_or(after.len()).min(WINDOW);
        out.push(&after[..end]);
        rest = &after[end..];
    }
    out
}

/// Tokens whose presence in a DSN window BEFORE the `return;` mean the branch
/// fails closed (or resolves the DSN from the fixture) instead of skipping:
/// a panic, a failing expectation, or a fixture/provisioning call.
const FAIL_CLOSED: [&str; 5] = ["panic!", ".expect(", "unwrap", "provision", "test_db"];

/// AC2's forbidden shape: a mention of the DSN var whose window bails out
/// with `return;` before anything fails or provisions — the silent no-op
/// (missing DSN => test returns Ok) that must be gone.
fn has_silent_dsn_skip(src: &str) -> bool {
    windows(src, DSN_VAR).iter().any(|w| {
        let failed = FAIL_CLOSED.iter().filter_map(|t| w.find(t)).min();
        match (w.find("return;"), failed) {
            (Some(r), Some(f)) => r < f,
            (Some(_), None) => true,
            (None, _) => false,
        }
    })
}

/// AC3's forbidden shape: a live-hub probe whose window reaches a `return;`
/// — the refusal surfaces as a green skip instead of a failure.
fn refusal_is_silent(src: &str) -> bool {
    windows(src, HUB_PROBE)
        .iter()
        .any(|w| w.contains("return;"))
}

/// AC3's required shape when a suite keeps its own inline refusal: the probe
/// window panics (or at least never returns green).
fn has_loud_refusal(src: &str) -> bool {
    windows(src, HUB_PROBE)
        .iter()
        .any(|w| match (w.find("return;"), w.find("panic!")) {
            (Some(r), Some(p)) => p < r,
            (None, Some(_)) => true,
            _ => false,
        })
}

/// A suite either refuses a live-hub DSN loudly itself, or routes its DSN
/// through the shared fixture module — which refuses centrally. Silent
/// green is the one shape forbidden on every path.
fn refusal_is_enforced(src: &str) -> bool {
    if src.contains(HUB_PROBE) {
        !refusal_is_silent(src)
    } else {
        src.contains("modcommon;") && src.contains("common::")
    }
}

/// Per-run unique naming: the tokens any run-unique derivation flattens to.
const RUN_UNIQUE: [&str; 7] = [
    "process::id",
    "SystemTime",
    "nanos",
    "uuid",
    "run_id",
    "timestamp",
    "RUN_ID",
];

// --- AC1 --------------------------------------------------------------------

#[test]
fn ac1_the_shared_compose_fixture_exists_and_provisions_a_dedicated_test_database() {
    let compose = flat(&read(FIXTURE_COMPOSE));
    assert!(
        compose.contains("postgres"),
        "the fixture stack must run Postgres — that is the test database the suites need"
    );
    assert!(
        compose.contains("healthcheck"),
        "the fixture stack must gate readiness with a healthcheck — the CXA-B115 lesson: \
         a db that is 'up' but unauthenticated is not up"
    );
    assert!(
        windows(&compose, "name:")
            .iter()
            .any(|w| w.contains("test")),
        "the fixture stack must declare its own compose project name marking it a test \
         fixture, so concurrent runs and developer stacks never collide"
    );
    assert!(
        !compose.contains("external:true"),
        "the fixture stack must never mount an external volume — external volumes are the \
         LIVE stores (deploy/local-infra), and the fixture must stay disposable"
    );

    let fixture = flat(&read(FIXTURE_MODULE));
    assert!(
        fixture.contains("docker"),
        "the fixture driver must detect/drive docker — provisioning is its job, not the \
         operator's"
    );
    assert!(
        fixture.contains("test-pg.compose.yml"),
        "the fixture driver must run the shared compose stack ({FIXTURE_COMPOSE})"
    );
    assert!(
        fixture.contains(DSN_VAR),
        "the fixture driver must export/honor {DSN_VAR} itself — no manually exported DSN \
         may be needed (AC1)"
    );
}

#[test]
fn ac1_the_contract_suite_executes_through_the_shared_fixture_without_a_manual_dsn() {
    let contract = flat(&read(DB_SUITES[0]));
    assert!(
        contract.contains("modcommon;"),
        "sql_store_contract must route its DSN through the shared fixture module (mod \
         common;) — today it gates every test on a manually exported DSN"
    );
    assert!(
        !has_silent_dsn_skip(&contract),
        "sql_store_contract still returns Ok when the DSN is absent — the suite must \
         EXECUTE via the fixture, not skip (AC1)"
    );
    for test in [
        "fnsql_store_satisfies_contract",
        "fnsql_store_rejects_stale_revision_write_with_conflict",
        "fnsql_store_delete_purges_state_and_coordination_for_the_project_only",
    ] {
        assert!(
            contract.contains(test),
            "the FULL sql_store_contract suite must run — {test} must not vanish to make \
             the suite green (AC1)"
        );
    }
}

// --- AC2 --------------------------------------------------------------------

#[test]
fn ac2_a_missing_or_unreachable_test_database_fails_red_naming_the_fixture_and_dsn() {
    for suite in DB_SUITES {
        let src = flat(&read(suite));
        assert!(
            !has_silent_dsn_skip(&src),
            "{suite} still returns Ok when the test database is missing — the silent \
             no-op path must be gone: an unprovisionable/unreachable DB fails RED naming \
             the fixture/DSN (AC2)"
        );
    }
    let fixture = flat(&read(FIXTURE_MODULE));
    assert!(
        fixture.contains(DSN_VAR) && fixture.contains("test-pg.compose.yml"),
        "the fixture driver's failure output must name the fixture stack and the DSN var \
         — an operator must be able to tell WHAT was not provisioned (AC2)"
    );
    assert!(
        FAIL_CLOSED.iter().any(|t| fixture.contains(t)),
        "the fixture driver must fail loudly (panic/expect) when the test database \
         cannot be provisioned or reached — never skip green (AC2)"
    );
}

// --- AC3 --------------------------------------------------------------------

#[test]
fn ac3_a_live_hub_dsn_is_refused_loudly_by_every_db_backed_suite() {
    for suite in DB_SUITES {
        let src = flat(&read(suite));
        assert!(
            refusal_is_enforced(&src),
            "{suite} does not enforce the live-hub refusal (CXA-F327 AC3): it must either \
             abort loudly on {HUB_PROBE} (a green `return;` skip is exactly the bug) or \
             route its DSN through the shared fixture, which refuses centrally"
        );
        if src.contains(HUB_PROBE) {
            let probe_at = src.find(HUB_PROBE).expect("probe mention checked above");
            let first_write = src.find("::connect(");
            if let Some(write_at) = first_write {
                assert!(
                    probe_at < write_at,
                    "{suite} connects before the live-hub probe — the refusal must happen \
                     BEFORE any store contact so a refused run writes nothing and the row \
                     count is unchanged (AC3)"
                );
            }
        }
    }
}

// --- AC4 --------------------------------------------------------------------

#[test]
fn ac4_without_docker_the_db_backed_tests_report_an_explicit_skip() {
    let fixture = flat(&read(FIXTURE_MODULE));
    assert!(
        fixture.contains("docker"),
        "the fixture driver must detect docker presence — a docker-less environment is a \
         distinct, handled situation (AC4)"
    );
    assert!(
        fixture.contains("skip"),
        "the fixture driver must emit an explicit skip (reason visible in the run \
         summary) when docker is absent — today there is no such branch, so a docker-less \
         run is indistinguishable from a pass (AC4)"
    );
}

#[test]
fn ac4_with_docker_but_a_failed_fixture_stack_the_suite_is_red() {
    let fixture = flat(&read(FIXTURE_MODULE));
    assert!(
        FAIL_CLOSED.iter().any(|t| fixture.contains(t)),
        "docker present but the test-pg stack failing to come up must panic the suite \
         RED — a needed-but-absent database is never silently green (AC4)"
    );
}

// --- AC5 --------------------------------------------------------------------

#[test]
fn ac5_runs_are_isolated_per_run_and_never_contaminate_each_other() {
    let fixture = flat(&read(FIXTURE_MODULE));
    assert!(
        fixture.contains("schema") || fixture.contains("search_path"),
        "the fixture driver must namespace each run (per-run project/schema) on the \
         shared fixture database — back-to-back runs must not trip over leftover state \
         (AC5)"
    );
    assert!(
        RUN_UNIQUE.iter().any(|t| fixture.contains(t)),
        "the fixture driver's run namespace must be derived from a run-unique value — \
         two concurrent runs on one shared fixture must not share a namespace (AC5)"
    );
    // Green control: the suites ALREADY derive their project/user/key ids
    // from per-run unique values; the fixture half above is the red half.
    for suite in DB_SUITES {
        let src = flat(&read(suite));
        assert!(
            RUN_UNIQUE.iter().any(|t| src.contains(t)),
            "{suite} must keep deriving its project/user/key names from a per-run unique \
             value — a fixed id would contaminate the shared fixture across runs (AC5)"
        );
    }
}

// --- Controls (green today; they pin the required semantics) -----------------

/// What the fixture driver must decide, for every situation the ACs name.
/// `Skip` is lawful ONLY without docker (AC4); every other failure is RED
/// (AC2/AC4); a live-hub DSN is always RED (AC3); a provisioned dedicated
/// database RUNS (AC1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Docker {
    Available,
    Absent,
}

/// What the fixture's probe found when asked for a test database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Ready,
    Unreachable,
    ProvisionFailed,
    LiveHubDsn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Run,
    Skip,
    FailRed,
}

fn fail_closed(docker: Docker, probe: Probe) -> Verdict {
    match (docker, probe) {
        (Docker::Absent, _) => Verdict::Skip,
        (_, Probe::Ready) => Verdict::Run,
        // Live hub (AC3), unreachable (AC2), provision-failed (AC4b): every
        // dockerful failure is red — a needed-but-absent database is never
        // silently green.
        (_, Probe::LiveHubDsn | Probe::Unreachable | Probe::ProvisionFailed) => Verdict::FailRed,
    }
}

#[test]
fn the_fail_closed_policy_table_pins_a_verdict_for_every_fixture_situation() {
    // AC1: provisioned dedicated database -> run the suite.
    assert_eq!(
        fail_closed(Docker::Available, Probe::Ready),
        Verdict::Run,
        "a provisioned test database must run the DB-backed tests"
    );
    // AC4a: no docker -> explicit skip (the ONLY lawful green non-run).
    assert_eq!(
        fail_closed(Docker::Absent, Probe::ProvisionFailed),
        Verdict::Skip,
        "without docker the fixture cannot even try — explicit skip with reason"
    );
    // AC4b: docker present, stack failed -> red, never silent green.
    assert_eq!(
        fail_closed(Docker::Available, Probe::ProvisionFailed),
        Verdict::FailRed,
        "a needed-but-absent fixture stack with docker available is red"
    );
    // AC2: provisioned stack but unreachable database -> red.
    assert_eq!(
        fail_closed(Docker::Available, Probe::Unreachable),
        Verdict::FailRed,
        "an unreachable test database fails red naming the fixture/DSN"
    );
    // AC3: configured DSN points at a live hub -> red, writes nothing.
    assert_eq!(
        fail_closed(Docker::Available, Probe::LiveHubDsn),
        Verdict::FailRed,
        "a live-hub DSN aborts loudly as a failure, not a green skip"
    );
}

#[test]
fn the_source_scanners_catch_the_silent_patterns_they_forbid() {
    // The exact silent no-op the suites carry today must stay caught.
    let silent_skip = r#"let Ok(dsn) = std::env::var("COXAGENT_TEST_PG_DSN") else {
        eprintln!("COXAGENT_TEST_PG_DSN unset - skipping");
        return;
    };"#;
    assert!(
        has_silent_dsn_skip(&flat(silent_skip)),
        "the scanner must catch the missing-DSN => return Ok pattern"
    );
    // The fail-closed replacement must stay accepted.
    let fail_closed_dsn = r#"let dsn = std::env::var("COXAGENT_TEST_PG_DSN")
        .unwrap_or_else(|_| common::test_db());"#;
    assert!(
        !has_silent_dsn_skip(&flat(fail_closed_dsn)),
        "the scanner must not flag routing the DSN through the fixture"
    );
    // The green-skip refusal (today's shape) must stay caught.
    let green_refusal = r#"if common::is_live_hub_db(&dsn).await {
        eprintln!("points at a LIVE hub database - refusing");
        return;
    }"#;
    assert!(
        refusal_is_silent(&flat(green_refusal)),
        "the scanner must catch a live-hub refusal that returns green"
    );
    // A loud refusal (panic) must stay accepted.
    let loud_refusal = r#"if common::is_live_hub_db(&dsn).await {
        panic!("{DSN_VAR} points at a LIVE hub database - refusing, writes nothing");
    }"#;
    assert!(
        !refusal_is_silent(&flat(loud_refusal)) && has_loud_refusal(&flat(loud_refusal)),
        "the scanner must accept a live-hub refusal that fails the test"
    );
}
