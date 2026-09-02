//! CXA-F326 repo gate — the DSN-gated Postgres integration tests share ONE
//! fail-closed test-environment guard, and no gated test may bypass it.
//!
//! A refusal that prints to stderr and returns is indistinguishable from a
//! real run in CI, and a private copy of the check drifts: the KV/doc contract
//! tests shipped with no live-hub refusal at all while the SQL store contract
//! grew a second probe of its own. So the guard — the env reads, the skip
//! story, the cannot-verify error and the live-hub refusal — lives once, in
//! `infrastructure/tests/common/mod.rs`, and this gate fails the build the day
//! any gated test grows its own divergent copy again.
//!
//! (The CI-level half — the `integration` job that runs these `#[ignore]`d
//! suites against ephemeral service containers — is pinned in
//! `ci_availability_gate.rs`.)

use std::path::PathBuf;

/// Every integration test gated on the shared guard (the SQL store contract,
/// the KV/doc contract, distributed coordination, auth harvest).
const GATED: &[&str] = &[
    "sql_store_contract.rs",
    "kv_doc_contract.rs",
    "distributed_coord.rs",
    "sql_auth_token_harvest.rs",
];

/// The one shared guard every gated test must route through.
const GUARD: &str = "common/mod.rs";

/// Test files allowed to read the test env themselves. EMPTY and shrink-only:
/// a name here is a standing exception to the fail-closed barrier, so it may
/// only be removed, never added to.
const GRANDFATHERED: &[&str] = &[];

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../infrastructure/tests")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} — the gate needs the repo checkout: {e}",
            path.display()
        )
    })
}

/// The gate pins code, not prose: comment-only lines are dropped, so a doc
/// comment can neither satisfy a "must name" pin nor falsely trip a "must not
/// carry" one. String literals survive — the messages live in strings.
fn code(src: &str) -> String {
    src.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does this test source read the test environment itself instead of routing
/// through the shared guard? The env var names must appear in the guard's
/// code only; a file that grabs the DSN directly decides safety on its own,
/// which is exactly the drift that left the KV/doc family unguarded.
fn reads_env_directly(src: &str) -> bool {
    let code = code(src);
    code.contains("COXAGENT_TEST_PG_DSN") || code.contains("COXAGENT_TEST_REDIS_URL")
}

/// CXA-F326 AC: the guard itself must fail closed and speak the three
/// distinct outcomes — a live-hub refusal that names `COXAGENT_TEST_PG_DSN`,
/// the explicit 'cannot verify the target is not a live hub' error, and a
/// skip that says 'skipped' — so a skip stays an observable outcome distinct
/// from both a pass and a refusal.
#[test]
fn the_shared_guard_owns_the_fail_closed_messages() {
    let guard = code(&read(GUARD));

    assert!(
        guard.contains("COXAGENT_TEST_PG_DSN"),
        "the shared guard must name COXAGENT_TEST_PG_DSN in its refusal — a refusal \
         that does not say which variable to unset sends the operator debugging \
         the wrong thing"
    );
    assert!(
        guard.contains("LIVE hub") && guard.contains("refus"),
        "the shared guard must state that it refused a LIVE hub database — a \
         printed refusal that reads like a skip is how the incident shipped"
    );
    assert!(
        guard.contains("cannot verify the target is not a live hub"),
        "the shared guard must fail with 'cannot verify the target is not a live \
         hub' on connection or probe error — answering 'not live' lets the \
         destructive tests run against an unverified database"
    );
    assert!(
        guard.contains("skipped"),
        "the shared guard must emit an explicit 'skipped' message when the env \
         vars are unset — an explicit skip must stay a distinct, observable \
         outcome from both a pass and a live-hub refusal"
    );
}

/// CXA-F326 AC: no gated test keeps its own copy of the check — not a private
/// probe, not a private refusal message. A second copy is how the KV/doc
/// contract ended up with no refusal at all.
#[test]
fn no_gated_test_keeps_its_own_copy_of_the_check() {
    for file in GATED {
        let src = code(&read(file));
        assert!(
            !src.contains("fn is_live_hub_db"),
            "{file} defines its own live-hub check — the shared guard in \
             infrastructure/tests/{GUARD} is the only copy"
        );
        assert!(
            !src.contains(", \"cxa\")"),
            "{file} connects to the hub project 'cxa' itself — probing liveness \
             is the shared guard's decision, and a destructive test must never \
             touch the hub project"
        );
        assert!(
            !src.contains("LIVE hub"),
            "{file} carries its own live-hub refusal message — the shared guard \
             owns the refusal, or the copies drift apart again"
        );
    }
}

/// CXA-F326 AC: every DSN-gated test routes through the shared guard — the
/// KV/doc contract tests, which historically had no live-hub refusal at all,
/// must refuse a live-hub DSN with the same fail-loudly behaviour and message
/// as the SQL store contract tests.
#[test]
fn every_gated_test_gates_through_the_shared_guard() {
    for file in GATED {
        let src = code(&read(file));
        assert!(
            src.contains("mod common;"),
            "{file} must gate through the shared guard in \
             infrastructure/tests/{GUARD} — it does not use it at all"
        );
    }
}

/// CXA-F326 AC, the `COXAGENT_TEST_REDIS_URL` half: where the distributed
/// coordination tests require Redis too, an unset Redis URL must stay an
/// explicit, observable skip — the message names the variable. It lives with
/// the guard, never nowhere.
#[test]
fn the_redis_gate_stays_an_explicit_named_skip() {
    let coord = read("distributed_coord.rs");
    let guard = read(GUARD);
    assert!(
        coord.contains("COXAGENT_TEST_REDIS_URL") || guard.contains("COXAGENT_TEST_REDIS_URL"),
        "the distributed coordination tests must keep naming COXAGENT_TEST_REDIS_URL \
         in their skip — an unexplained silence reads as a pass in CI"
    );
}

/// CXA-F326 AC: the gated tests are `#[ignore]`d with a reason that starts
/// with "skipped", so ordinary CI without a database skips them explicitly
/// (libtest prints the reason) and stays green — while running them via
/// `--ignored` demands an explicit, verified target from the guard.
#[test]
fn every_gated_test_is_ignored_so_ordinary_ci_skips_them_explicitly() {
    for file in GATED {
        let src = code(&read(file));
        assert!(
            src.contains("#[ignore"),
            "{file} must #[ignore] its DSN-gated tests — a test that silently \
             returns when the env is unset is reported `ok` in CI, the exact \
             false green CXA-F326 closes"
        );
        assert!(
            src.contains("#[ignore = \"skipped:"),
            "{file}'s ignore reason must start with 'skipped:' — the ignore line \
             is the only skip message ordinary CI ever prints, so it must say \
             what was skipped and why"
        );
    }
}

/// CXA-F326 AC: the guard owns the env vars — no gated test reads
/// `COXAGENT_TEST_PG_DSN` or `COXAGENT_TEST_REDIS_URL` itself (comment lines
/// don't count). A new gated test that grabs the DSN directly is caught
/// unless grandfathered — and the grandfather list is empty and shrink-only.
#[test]
fn no_gated_test_reads_the_test_env_directly() {
    for file in GATED {
        if GRANDFATHERED.contains(file) {
            continue;
        }
        let src = read(file);
        assert!(
            !reads_env_directly(&src),
            "{file} reads the test env itself — every env decision must route \
             through the shared guard in infrastructure/tests/{GUARD}, or the \
             fail-closed barrier springs a second, unguarded door"
        );
    }
}

/// The env-read pin actually bites: a source that reads the var in code is
/// flagged, while a mention confined to comments is not (the guard's own run
/// instructions live in doc comments).
#[test]
fn an_env_reading_test_file_is_caught_unless_grandfathered() {
    let mutant = "\
#[tokio::test]
async fn t() {
    let Ok(dsn) = std::env::var(\"COXAGENT_TEST_PG_DSN\") else { return; };
}
";
    assert!(
        reads_env_directly(mutant),
        "the gate must catch a test file that reads COXAGENT_TEST_PG_DSN itself"
    );

    let comment_only = "\
// COXAGENT_TEST_PG_DSN mention in a comment is not a read
async fn t() {}
";
    assert!(
        !reads_env_directly(comment_only),
        "a mention in a comment is documentation, not an env read"
    );
}

/// The grandfather list is shrink-only: an entry is justified only while its
/// file still reads the env itself — the moment the file routes through the
/// guard, the stale entry must be removed.
#[test]
fn grandfathered_entries_are_still_needed() {
    for file in GRANDFATHERED {
        let src = read(file);
        assert!(
            reads_env_directly(&src),
            "grandfathered entry {file} no longer reads the env itself — remove \
             it from the list (the list is shrink-only)"
        );
    }
}
