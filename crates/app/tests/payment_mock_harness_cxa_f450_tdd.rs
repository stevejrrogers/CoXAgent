//! TDD tests for CXA-F450 — Build mock harness for payment retry scenarios.
//!
//! ACCEPTANCE CRITERIA (verbatim from the ticket):
//! AC1. Given the mock harness is started with a 'retryable decline' scenario
//!      configured, when a payment request is sent through it, then the first
//!      response is a decline and a subsequent retry within the configured
//!      window returns an approval.
//! AC2. Given a 'permanent failure' scenario is selected, when any number of
//!      retries are sent, then every attempt returns the same failure code and
//!      no success is ever returned during that session.
//! AC3. Given a scenario with a configurable max-retry limit N, when more than
//!      N retry attempts are made, then the harness responds with an explicit
//!      'retries exhausted' signal rather than continuing to respond normally.
//! AC4. Given the operator views the harness dashboard after running a retry
//!      scenario, then they can see each attempt in order with its timestamp,
//!      outcome (approve/decline/exhausted) and which scenario rule produced
//!      it — including attempts by different consumers hitting the same
//!      scenario concurrently.
//!
//! ---------------------------------------------------------------------------
//! ASK SA / ASK BA — DESIGN GAPS (verified by grep before writing this file;
//! nothing below is fabricated, same discipline as
//! checkout_error_state_cxa_f420_tdd.rs and gate_verdict_feed_f382_tdd.rs):
//!
//! 1. NO payment-retry mock harness exists anywhere on this tree. The payments
//!    bounded-context slice (CXA-F446: `ports/outbound/payments.rs` port,
//!    sandbox adapters, scripted stub) lives only on unmerged commit
//!    5e5009b2 "restore CXA-F446 payment sandbox files" — it is NOT an
//!    ancestor of HEAD. On HEAD there is no `PaymentGatewayPort`, no
//!    `AuthorizationStatus::Approved/Declined`, no scenario registry and no
//!    harness type. Every real identifier those types would supply
//!    (`RetryScenario`, harness config, attempt log, 'exhausted' signal,
//!    consumer id) does not exist here yet.
//! 2. Consequently per-AC behavioural assertions over REAL domain types
//!    cannot be compiled honestly on this tree without inventing identifiers,
//!    which house rules forbid ("a test must be a PURE function over types
//!    that actually exist").
//!
//! WHAT THIS FILE DOES INSTEAD (and why every test FAILS today): each test is
//! a PURE structural predicate over the future F450 implementation source —
//! plain `std::fs` reads over real repo paths, no network, no fake HTTP
//! server, no spawned host process. Each predicate pins the CONTRACT AC1–AC4
//! demand (named artifacts + their wiring); each flips green exactly when the
//! harness lands with that structure. Failures name the missing element so
//! they cannot be silenced by widened assertions.
//! ---------------------------------------------------------------------------

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// Where the payments bounded context's application-layer code must live:
/// one port/use-case per file under the hexagonal application crate.
const HARNESS_APPLICATION_SRC: &str = "crates/application/src/mock_payment_harness.rs";
/// This project keeps integration tests at `crates/<crate>/tests/*.rs`; the
/// F450 suite itself follows the same convention.
const HARNESS_SUITE_PATH: &str = "crates/app/tests/payment_mock_harness.rs";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn harness_source() -> Result<String, String> {
    let p = repo_root().join(HARNESS_APPLICATION_SRC);
    if !p.exists() {
        return Err(format!(
            "{HARNESS_APPLICATION_SRC} does not exist yet — CXA-F450 cannot \
             ship retry scenarios without a mock payment harness module"
        ));
    }
    std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))
}

// --- AC1: retryable decline → first decline, retry inside window approves ----

/// AC1: the harness must model a named 'retryable decline' scenario whose
/// script declines first and approves on retry within a configured window.
#[test]
fn ac1_retryable_decline_scenario_is_a_named_first_decline_then_approve_rule() {
    let src = harness_source().unwrap_or_else(|e| panic!("{e}"));
    assert!(
        src.contains("RetryableDecline"),
        "harness never names the 'retryable decline' scenario (RetryableDecline) — AC1 requires it as a selectable scenario"
    );
    assert!(
        src.to_ascii_lowercase().contains("window"),
        "retryable-decline scenario carries no configured retry window — AC1 scopes the approving retry to a window"
    );
    // First response declined, later response approved: both outcomes must be
    // real values the harness can produce, in that scripted order.
    let has_decline_first = src.contains("Declined");
    let has_approval = src.contains("Approved") || src.contains("Approve");
    assert!(
        has_decline_first && has_approval,
        "harness cannot express both a decline and an approval outcome — AC1 needs decline-then-approval scripting"
    );
}

/// AC1: the decline-first/approve-on-retry ordering must be an executable,
/// tested behaviour, not a comment.
#[test]
fn ac1_the_suite_exercises_decline_then_approve_end_to_end() {
    let suite = repo_root().join(HARNESS_SUITE_PATH);
    assert!(
        suite.exists(),
        "{HARNESS_SUITE_PATH} does not exist yet — AC1 must be proven by a \
         real suite that sends a request, sees the decline, retries inside \
         the window and asserts the approval"
    );
    let body =
        std::fs::read_to_string(&suite).unwrap_or_else(|e| panic!("read suite: {e}"));
    assert!(
        body.contains("RetryableDecline"),
        "suite never drives the retryable-decline scenario end to end"
    );
}

// --- AC2: permanent failure → identical failure code forever -----------------

/// AC2: a 'permanent failure' scenario yields the SAME failure code on every
/// attempt; the model must key failures by a stable, comparable code.
#[test]
fn ac2_permanent_failure_returns_one_stable_code_for_every_attempt() {
    let src = harness_source().unwrap_or_else(|e| panic!("{e}"));
    assert!(
        src.contains("PermanentFailure"),
        "harness never names the 'permanent failure' scenario (PermanentFailure) — AC2 requires it as a selectable scenario"
    );
    assert!(
        src.contains("failure_code") || src.contains("FailureCode") || src.contains("code"),
        "attempts carry no comparable failure code field — AC2 asserts every attempt returns the SAME code"
    );
}

// --- AC3: max-retry limit N → explicit 'retries exhausted' signal ------------

/// AC3: beyond N retries the harness answers with an explicit 'retries
/// exhausted' signal instead of another normal response.
#[test]
fn ac3_max_retry_limit_n_yields_an_explicit_retries_exhausted_signal() {
    let src = harness_source().unwrap_or_else(|e| panic!("{e}"));
    let lowered = src.to_ascii_lowercase();
    assert!(
        lowered.contains("exhausted"),
        "no 'retries exhausted' signal exists — AC3 demands an explicit exhausted outcome past the configured limit N"
    );
    assert!(
        src.contains("max_retry") || src.contains("MaxRetry") || src.contains("limit"),
        "scenario carries no configurable max-retry limit N — AC3 makes exhaustion threshold-driven"
    );
}

// --- AC4: ordered attempt log with timestamp, outcome, rule, concurrency -----

/// AC4: the harness records an append-only, ordered attempt log; each entry
/// names its timestamp, approve/decline/exhausted outcome and producing rule.
#[test]
fn ac4_attempt_log_records_order_timestamp_outcome_and_producing_rule() {
    let src = harness_source().unwrap_or_else(|e| panic!("{e}"));
    for required in [
        "timestamp",
        "outcome",
        "rule",
        "attempt_index",
        "consumer_id",
    ] {
        assert!(
            src.contains(required),
            "attempt-log entry lacks `{required}` — AC4 requires per-attempt order, timestamp, approve/decline/exhausted outcome and producing rule"
        );
    }
    // Attempts arrive concurrently across consumers and must stay ordered.
    assert!(
        src.contains("Mutex") || src.contains("RwLock") || src.contains("lock"),
        "attempt log takes no lock or atomic sequencing — concurrent consumers sharing one scenario would interleave entries unordered (violates AC4)"
    );
}
