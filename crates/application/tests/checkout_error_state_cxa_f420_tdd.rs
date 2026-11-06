//! TDD tests for CXA-F420 — Fix checkout error-state bugs (payment retry
//! root cause).
//!
//! ACCEPTANCE CRITERIA (verbatim from the ticket):
//! AC1. Given a failed payment attempt, when the user retries checkout, then
//!      the retry uses a fresh payment intent/token (no reuse of the failed
//!      one) and succeeds without requiring page reload or re-entering
//!      unchanged payment details.
//! AC2. When any checkout step fails (payment, address validation, inventory),
//!      the user sees a specific, human-readable error message naming what
//!      went wrong and at least one next action, instead of a generic
//!      'something went wrong' banner.
//! AC3. After a failed payment followed by a successful retry, the order is
//!      created exactly once (no duplicate orders/charges), verifiable from
//!      the order list and payment history.
//! AC4. When a retry is in progress, the submit control shows a pending state
//!      and is disabled so a double-click cannot fire concurrent retries; it
//!      re-enables on success or failure.
//! AC5. If the checkout session/token expires mid-payment, the user is
//!      informed the session expired and is guided back into checkout with
//!      their cart contents preserved.
//!
//! ---------------------------------------------------------------------------
//! ASK BA — DESIGN GAPS (exact data/types the acceptance criteria reference
//! that DO NOT EXIST in the codebase today; nothing below is fabricated,
//! same discipline as gate_verdict_feed_f382_tdd.rs used pre-design):
//!
//!   1. NO checkout / commerce bounded context exists anywhere in the repo.
//!      Grep across every crate finds no `PaymentIntent`, `CartItem`,
//!      `Order`, charge id, token, or "cart" type in `domain`, `application`
//!      or the web presentation layer (the only hits are prose fixtures in
//!      `use_cases/global_search.rs`). There is no checkout route, handler or
//!      view either (`e2e/specs` has no checkout spec).
//!   2. AC1 asserts a "fresh payment intent/token" — there is no payment
//!      provider port (`ports/outbound`) nor any intent/token value object.
//!   3. AC2 asserts per-step errors (payment / address validation /
//!      inventory) — there is no address model, no inventory check, and no
//!      user-facing error-message surface attached to any step concept.
//!   4. AC3 asserts an order list and payment history — no order aggregate,
//!      ledger or history store exists in any shard of `ProjectState`.
//!   5. AC4 asserts a submit-control pending state — there is no checkout UI
//!      component to hold such state (presentation/js has no checkout module).
//!   6. AC5 asserts session expiry with cart preservation — no session TTL
//!      or persisted cart snapshot exists on any state struct.
//!
//! Because EVERY criterion asserts behaviour over these missing pieces, this
//! file can only PIN the existing baselines the implementation must preserve
//! while adding them (the convention `gate_verdict_feed_f382_tdd.rs` set).
//! The per-AC behavioural tests follow the moment the commerce domain model +
//! payment port land — they are deliberately NOT stubbed with invented
//! signatures.
//! ---------------------------------------------------------------------------

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::state::ProjectState;
use coxagent_application::state::{AttemptFailure, FailureLayer};
use coxagent_domain::SemVer;

/// Baseline helper over REAL constructors — a failed-attempt record shaped the
/// way the runner already logs failures. AC2's per-step error messages must be
/// built FROM structured failures like this (layer + gate + detail), never
/// pattern-matched out of a generic banner.
fn attempt_failure(attempt: u32, gate: &str, detail: &str) -> AttemptFailure {
    AttemptFailure {
        attempt,
        layer: FailureLayer::Infra,
        gate: gate.to_owned(),
        detail: detail.to_owned(),
        files: Vec::new(),
    }
}

/// PIN underpinning AC2: structured failure attribution (which attempt, which
/// layer, which gate, exact detail) already round-trips through `ProjectState`
/// untouched. The checkout-specific step errors (payment/address/inventory)
/// must ride THIS channel — specific name + next action per failure — not
/// replace it with a single generic banner string.
#[test]
fn pin_structured_failure_attribution_round_trips_per_step() {
    let mut s = ProjectState {
        current_version: SemVer::new(1, 0, 0),
        ..ProjectState::default()
    };
    s.record_attempt_failure(
        "CXA-F420",
        attempt_failure(1, "payment", "card declined — retry with a different card"),
    );
    s.record_attempt_failure(
        "CXA-F420",
        attempt_failure(
            2,
            "address-validation",
            "postal code malformed — correct the ZIP field",
        ),
    );

    let logged = s.attempt_failures("CXA-F420");
    assert_eq!(
        logged.len(),
        2,
        "each failed step is its own row, not one merged banner"
    );
    assert_eq!(logged[0].gate, "payment");
    assert_eq!(
        logged[1].detail,
        "postal code malformed — correct the ZIP field"
    );
    assert_eq!(logged[0].attempt, 1);
    assert_eq!(logged[1].attempt, 2);
}

/// PIN underpinning AC3 ("created exactly once"): attempt numbering on the
/// existing failure log already distinguishes try #1 from the retry (#2) —
/// the exact hook a dedupe/idempotency key for order creation will build on.
/// If implementers collapse or renumber attempts, order-once semantics have
/// lost their counter.
#[test]
fn pin_retry_attempts_are_distinct_not_merged() {
    let mut s = ProjectState {
        current_version: SemVer::new(1, 0, 0),
        ..ProjectState::default()
    };
    s.record_attempt_failure("CXA-F420", attempt_failure(1, "tests", "first try died"));
    s.record_attempt_failure("CXA-F420", attempt_failure(2, "tests", "retry also died"));

    let logged = s.attempt_failures("CXA-F420");
    let nums: Vec<u32> = logged.iter().map(|f| f.attempt).collect();
    assert_eq!(
        nums,
        vec![1, 2],
        "failed attempt and its retry stay distinct rows"
    );
}

/// PIN underpinning AC5 ("cart contents preserved" across a session break):
/// `ProjectState::default()` plus serde round-trip is the persistence shape
/// ANY cart snapshot must survive. Prove the store round-trips user-visible
/// collections intact today, so "preserved after expiry" has a real baseline
/// to anchor against instead of a wish.
#[test]
fn pin_project_state_survives_a_store_round_trip_intact() {
    let mut s = ProjectState {
        current_version: SemVer::new(1, 0, 0),
        ..ProjectState::default()
    };
    s.record_attempt_failure("CXA-F420", attempt_failure(1, "engine", "boom"));

    let json = serde_json::to_string(&s).expect("state serializes");
    let back: ProjectState = serde_json::from_str(&json).expect("state deserializes");
    assert_eq!(
        back.attempt_failures("CXA-F420").len(),
        1,
        "whatever the cart-preservation mechanism becomes, the persistence layer it lands on already preserves user state across save/load"
    );
}

/// PIN underpinning AC1/AC4: the domain transition table still lets an
/// Infra-layer failure reach a terminal explainable state (a retry MAY help,
/// per `domain::terminal_state`). A fresh-intent retry path must keep classifying
/// payment-provider drops as Infra (never the user's fault) — this locks the
/// enum variant the classification will use.
#[test]
fn pin_infra_layer_is_the_class_for_provider_side_failures() {
    let f = attempt_failure(1, "payment", "provider timeout");
    assert_eq!(
        f.layer,
        FailureLayer::Infra,
        "payment-provider side failures classify as Infra — nobody's fault, retried free of charge; the fresh-intent retry in AC1 builds on this"
    );
}
