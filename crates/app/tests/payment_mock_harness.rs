//! End-to-end harness suite for CXA-F450: drives the
//! [`MockPaymentHarness`](coxagent_application::mock_payment_harness) — the
//! pure, offline stand-in for the flaky card-provider sandbox — through its
//! four acceptance paths. Pairs with `payment_mock_harness_cxa_f450_tdd.rs`
//! (the file-presence/contract guard); this suite executes what that one
//! only asserts exists.
//!
//! Scenario coverage: `ScenarioKind::RetryableDecline` (decline first,
//! approve on retry inside the window; plus its past-window edge) and
//! `ScenarioKind::PermanentFailure` (one stable code, never approves), both
//! under the configurable max-retries exhaustion signal.

use coxagent_application::mock_payment_harness::{
    MockPaymentHarness, PaymentStatus, RETRYABLE_CODE,
};

fn stamp(sec: u8) -> String {
    format!("2026-11-18T14:07:{sec:02}Z")
}

/// AC1: first response declines with a retryable code; a subsequent retry
/// inside the configured window approves.
#[test]
fn decline_then_approve_on_a_retry_within_the_window() {
    let h = MockPaymentHarness::retryable_decline(3, 10);
    let first = h.authorize("checkout-web", &stamp(0));
    assert_eq!(first.status, PaymentStatus::Declined);
    assert_eq!(first.failure_code.as_deref(), Some(RETRYABLE_CODE));

    let retry = h.authorize("checkout-web", &stamp(2));
    assert_eq!(
        retry.status,
        PaymentStatus::Approved,
        "a retry inside the window must approve"
    );
    // No failure code on success; attempt indexes line up with the log.
    assert!(retry.failure_code.is_none());
    assert_eq!(retry.attempt_index, 1);

    let log = h.attempts().unwrap_or_default();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].outcome, PaymentStatus::Declined);
    assert_eq!(log[1].outcome, PaymentStatus::Approved);
}

/// AC1 edge: past the closed window the refusal is distinct and stable
/// (`window_closed`) so callers can tell 'too late' from 'try again'.
#[test]
fn a_late_retry_is_refused_with_a_distinct_too_late_code() {
    let h = MockPaymentHarness::retryable_decline(1, 10);
    assert_eq!(h.authorize("c", &stamp(0)).status, PaymentStatus::Declined);
    assert_eq!(h.authorize("c", &stamp(1)).status, PaymentStatus::Approved);
    let late = h.authorize("c", &stamp(2));
    assert_eq!(late.status, PaymentStatus::Declined);
    assert_eq!(late.failure_code.as_deref(), Some("window_closed"));
}

/// AC2: permanent-failure scenario repeats ONE stable failure code on every
/// attempt and never approves within the session.
#[test]
fn permanent_failure_repeats_one_stable_code_and_never_approves() {
    let h = MockPaymentHarness::permanent_failure("issuer_unavailable", 5);
    for i in 0..5_u8 {
        let r = h.authorize("pos-terminal", &stamp(i));
        assert_eq!(r.status, PaymentStatus::Declined, "attempt {i}");
        assert_eq!(r.failure_code.as_deref(), Some("issuer_unavailable"), "attempt {i}: every refusal carries the same comparable code");
    }
    let log = h.attempts().unwrap_or_default();
    assert!(log.iter().all(|a| a.rule == "permanent_failure"));
}

/// AC3: when the configured max-retries N is spent, the explicit
/// RetriesExhausted signal replaces any normal response.
#[test]
 fn the_exhaustion_signal_fires_once_max_retries_is_spent() {
    let h = MockPaymentHarness::retryable_decline(5, 2);
    for i in 0..3_u8 {
        assert_ne!(
            h.authorize("c", &stamp(i)).status,
            PaymentStatus::RetriesExhausted,
            "attempt index {i} is still at or under max_retries=2"
        );
    }
    let exhausted = h.authorize("c", &stamp(3));
    assert_eq!(exhausted.status, PaymentStatus::RetriesExhausted);
    assert_eq!(exhausted.attempt_index, 3);
    assert!(exhausted.failure_code.is_none());
    assert_eq!(
        h.authorize("c", &stamp(4)).status,
        PaymentStatus::RetriesExhausted,
        "exhaustion persists; no reset without a new session"
    );
}
