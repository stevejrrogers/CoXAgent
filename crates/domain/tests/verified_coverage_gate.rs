//! CXA-F024 — Test-to-AC Traceability Matrix: the `Fixed -> Verified` gate.
//!
//! Red half of the TDD pair, encoded as pure functions over the real domain
//! types (no IO, no harness): today `transition_to(Status::Verified)` succeeds
//! unconditionally, so the two blocking tests FAIL for exactly the missing
//! behaviour — the coverage invariant the ticket must add. The two allow-tests
//! pin the conditional halves of the same ACs (all-covered verifies; criteria
//! deliberately emptied after clarification verify) so the future gate cannot
//! over-block.
//!
//! NOT encoded here, because no backing types exist in the codebase to assert
//! against (reported as design gaps, not fabricated): the Test Coverage tab's
//! per-criterion COVERED / NOT TESTED / PARTIALLY COVERED model with test-file
//! links, the keyword + fuzzy (Levenshtein ≤ 0.3) matching/scoring function,
//! and API request/response evidence counting as coverage. Any test naming
//! them would need invented identifiers and could not compile honestly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_domain::ticket::{TestCaseStatus, TicketType};
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId};

const AC_REPRO: &str = "repro steps are listed on the ticket";
const AC_ROOT_CAUSE: &str = "root cause is fixed at source";

/// A bug driven to `Fixed` — the only lifecycle that can reach `Verified` —
/// with its acceptance criteria synced into pending test cases.
fn fixed_bug(criteria: &[&str]) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new("BUG-CXA-F024").expect("id"),
        TicketType::Bug,
        "traceability gate fixture",
        "fixture",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    t.transition_to(Role::DevBug, Status::InProgress)
        .expect("claim");
    t.transition_to(Role::DevBug, Status::Fixed).expect("fix");
    t.set_acceptance_criteria(criteria.iter().map(|c| (*c).to_owned()).collect());
    t.ensure_test_cases_from_acceptance();
    t
}

fn mark(t: &mut Ticket, criterion: &str, passed: bool) {
    assert!(
        t.set_test_case_result(criterion, passed, None, None, "2026-08-28T00:00:00Z".into()),
        "case must exist for criterion: {criterion}"
    );
}

/// AC: tickets with any NOT TESTED acceptance criterion are blocked from
/// transitioning to VERIFIED — the invariant is enforced on the aggregate's
/// `transition_to`, so every caller (agent or human) hits the same gate.
#[test]
fn verified_is_blocked_while_a_criterion_is_not_tested() {
    let mut t = fixed_bug(&[AC_REPRO, AC_ROOT_CAUSE]);
    assert_eq!(
        t.test_cases()[1].status,
        TestCaseStatus::Pending,
        "fixture: a criterion with no verdict is NOT TESTED"
    );
    assert!(
        t.transition_to(Role::Test, Status::Verified).is_err(),
        "Verified must be blocked while a criterion has no verdict"
    );
    assert_eq!(
        t.status(),
        Status::Fixed,
        "a blocked transition must leave the ticket untouched"
    );
}

/// AC: tickets with any PARTIALLY COVERED criterion are blocked too. A
/// criterion whose only case FAILED is not demonstrated by a passing test —
/// whatever the exact label the matrix gives it (NOT TESTED or PARTIALLY
/// COVERED), both non-covered statuses block per the AC.
#[test]
fn verified_is_blocked_when_a_criterion_only_has_a_failing_case() {
    let mut t = fixed_bug(&[AC_REPRO, AC_ROOT_CAUSE]);
    mark(&mut t, AC_REPRO, true);
    mark(&mut t, AC_ROOT_CAUSE, false);
    assert!(
        t.transition_to(Role::Test, Status::Verified).is_err(),
        "Verified must be blocked while a criterion lacks a passing case"
    );
    assert_eq!(t.status(), Status::Fixed);
}

/// AC (conditional half): with EVERY criterion covered by a passing case the
/// transition is allowed — the gate reads coverage, it is not a blanket ban
/// on Verified.
#[test]
fn verified_is_allowed_when_every_criterion_is_covered() {
    let mut t = fixed_bug(&[AC_REPRO, AC_ROOT_CAUSE]);
    mark(&mut t, AC_REPRO, true);
    mark(&mut t, AC_ROOT_CAUSE, true);
    t.transition_to(Role::Test, Status::Verified)
        .expect("fully covered bug verifies");
    assert_eq!(t.status(), Status::Verified);
}

/// AC (edge case): acceptance criteria deliberately empty after BA
/// clarification allow the transition — no criteria, nothing to cover. The
/// emptied state is produced the only way the codebase can produce it:
/// `clarify` restates the description (it cannot touch criteria), then the
/// criteria list is cleared explicitly.
#[test]
fn verified_is_allowed_when_criteria_are_deliberately_emptied_after_clarification() {
    let mut t = fixed_bug(&[AC_REPRO]);
    t.clarify(
        Role::Ba,
        "Requirement restated; criteria deliberately left empty pending re-proposal.",
    )
    .expect("BA may restate");
    t.set_acceptance_criteria(Vec::new());
    assert!(t.acceptance_criteria().is_empty());
    t.transition_to(Role::Test, Status::Verified)
        .expect("no criteria to cover — the transition is allowed");
    assert_eq!(t.status(), Status::Verified);
}
