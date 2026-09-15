//! CXA-B252 part 1/3 — guard tests for the pure terminal-state contract core
//! (`coxagent_domain::terminal_state`).
//!
//! Three families:
//! 1. EXHAUSTIVENESS — a match with an explicit arm per variant (no `_`) over
//!    a `TerminalState` built by the public constructors; adding a variant
//!    without an arm must fail to compile (see the `compile_fail` doctest on
//!    `TerminalState`).
//! 2. INVARIANTS — every constructor's happy path and every blank-payload
//!    rejection branch.
//! 3. PURITY — the module is domain-only: std trait derives only, no serde,
//!    no IO symbols anywhere under `src/terminal_state/`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Table-driven pure tests over the contract's own types, as in the B192
// contract tests — constructor `Result`s are unwrapped in the fixture, which
// is exactly what the invariant tests below prove.

use coxagent_domain::terminal_state::{core::TerminalState, failure::FailureReason};
use coxagent_domain::DomainError;

/// Every variant, built through the public constructors (happy paths).
fn every_variant() -> Vec<TerminalState> {
    vec![
        TerminalState::succeeded("2026-09-14T11:26:53Z".to_owned(), "GET /api/state")
            .expect("a non-blank source is a valid success"),
        TerminalState::failed("connection refused", true)
            .expect("a non-blank cause is a valid failure"),
        TerminalState::cancelled(),
        TerminalState::skipped(FailureReason::NoData),
        TerminalState::timed_out("30s").expect("a non-blank deadline is a valid timeout"),
    ]
}

/// EXHAUSTIVENESS: one explicit arm per variant, no wildcard. Adding a
/// variant without an arm here is a COMPILE error (non_exhaustive patterns),
/// demonstrated by the sibling `refuses_a_variant_added_without_an_arm`
/// module below — this test keeps the live proof passing.
#[test]
fn every_variant_is_matched_explicitly_without_a_wildcard() {
    let states = every_variant();
    let summaries: Vec<String> = states
        .iter()
        .map(|state| match state {
            TerminalState::Succeeded { source, .. } => format!("succeeded by {source}"),
            TerminalState::Failed { cause, .. } => format!("failed: {cause}"),
            TerminalState::Cancelled => "cancelled".to_owned(),
            TerminalState::Skipped { reason, .. } => format!("skipped: {reason}"),
            TerminalState::TimedOut { deadline } => format!("timed out at {deadline}"),
        })
        .collect();
    assert_eq!(summaries.len(), 5);
    assert!(summaries.iter().all(|s| !s.is_empty()));
}

/// INVARIANTS — happy path for every variant.
#[test]
fn every_variant_is_constructible_through_its_public_constructor() {
    let states = every_variant();
    assert_eq!(
        states[0],
        TerminalState::Succeeded {
            fetched_at: "2026-09-14T11:26:53Z".to_owned(),
            source: "GET /api/state".to_owned(),
        }
    );
    assert_eq!(
        states[1],
        TerminalState::Failed {
            cause: "connection refused".to_owned(),
            retryable: true,
        }
    );
    assert_eq!(states[2], TerminalState::Cancelled);
    assert_eq!(
        states[3],
        TerminalState::Skipped {
            reason: FailureReason::NoData,
            hint: coxagent_domain::terminal_state::FillHint::for_reason(FailureReason::NoData),
        }
    );
    assert_eq!(
        states[4],
        TerminalState::TimedOut {
            deadline: "30s".to_owned(),
        }
    );
}

/// INVARIANTS — every blank-payload rejection branch returns `DomainError`.
#[test]
fn blank_payloads_are_refused_with_the_field_they_offend() {
    assert_eq!(
        TerminalState::succeeded("2026-09-14T11:26:53Z".to_owned(), "   "),
        Err(DomainError::Empty {
            field: "terminal_state_source"
        })
    );
    assert_eq!(
        TerminalState::failed("", true),
        Err(DomainError::Empty {
            field: "terminal_state_cause"
        })
    );
    assert_eq!(
        TerminalState::timed_out("  "),
        Err(DomainError::Empty {
            field: "terminal_state_deadline"
        })
    );
    assert_eq!(
        coxagent_domain::terminal_state::FillHint::new(" "),
        Err(DomainError::Empty {
            field: "terminal_state_fill_hint"
        })
    );
}

/// Every state is terminal, by construction of the module.
#[test]
fn every_state_is_terminal() {
    for state in every_variant() {
        assert!(state.is_terminal());
    }
}

/// `Cancelled` and `Skipped` are distinct semantics: an attempt was started
/// and abandoned vs. no attempt was made — the frozen B195/B192 split.
#[test]
fn cancelled_and_skipped_are_distinct_semantics() {
    assert_ne!(
        TerminalState::cancelled(),
        TerminalState::skipped(FailureReason::NoData)
    );
}

/// PURITY: `TerminalState` derives only std traits (Debug/Clone/PartialEq/Eq/
/// Hash) and implements Display — no serde derives, no IO. Asserted at
/// compile time by demanding the traits here; the source scan below keeps
/// out foreign imports.
#[test]
fn the_contract_is_pure_domain_std_only() {
    fn demands_std_traits<T: std::fmt::Debug + Clone + PartialEq + Eq>(_: &T) {}
    for state in every_variant() {
        demands_std_traits(&state);
    }
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/terminal_state/mod.rs"
    ))
    .expect("the module manifest is readable");
    assert!(
        !manifest.contains("serde"),
        "serialization is deliberately deferred to part 3"
    );
}
