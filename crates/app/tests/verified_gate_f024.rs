//! TDD contract for CXA-F024 ("Test-to-AC Traceability Matrix"), part 1 of 2:
//! the DOMAIN-INVARIANT acceptance criteria.
//!
//! This file COMPILES against the currently-shipped domain API and fails for
//! the right reason: it drives a real [`coxagent_domain::Ticket`] through legal
//! transitions and asserts the Verified-gating behaviour CXA-F024 must add (AC#3)
//! and its clarify-to-empty escape hatch (AC#4), neither of which exists yet, so
//! these assertions are RED on this build and go green once F024 lands.
//!
//// Coverage state is read here from observable fields that already exist: each
//// acceptance criterion has exactly one matching test case carrying a Pending /
//// Passed status. Mapping to F024's richer matrix model:
////   - all cases Pending                  -> every AC NOT TESTED      (AC#3 blocks)
////   - some Passed, some not              -> at least one uncovered    (AC#3 blocks)
////   - all cases Passed                   -> every AC covered         (verify allowed)
////   - zero criteria after clarify        -> nothing to cover         (AC#4 allows)
//
//// Part 2 (`traceability_f024_gate.rs`) pins AC#1/#2/#5 against the new
//// `coxagent_domain::coverage` surface F024 must add.

use coxagent_domain::{Complexity, Priority, Role, Status, TicketId};
use coxagent_domain::{Ticket};
use coxagent_domain::{TicketType};

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid id")
}

/// A bug driven Open -> InProgress -> Fixed with its remaining legal step toward VERIFIED open.
fn fixed_bug(id: &str) -> Ticket {
    let mut b = Ticket::new(
        tid(id),
        TicketType::Bug,
        "bug",
        "desc",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("new bug");
    b.transition_to(Role ::DevBug ,Status ::InProgress ).expect("claim");
    b.transition_to(Role ::DevBug ,Status ::Fixed ).expect("fixed");
    b
}

/// Seed one test case per acceptance criterion (as production does before TEST
/// records verdicts), then mark every one passing.
fn mark_all_passed(t: &mut Ticket) {
    t.ensure_test_cases_from_acceptance();
    for ac in t.acceptance_criteria().to_vec() {
        assert!(t.set_test_case_result(&ac, true, None, None, "t".into()));
    }
}

/// AC#3 -- a bug whose only acceptance criterion has no passing test case (NOT
/// TESTED) must be blocked from transitioning to VERIFIED.
#[test]
fn ac3_not_tested_criterion_blocks_transition_to_verified() {
    let mut b = fixed_bug("BUG-F024-1");
    b.set_acceptance_criteria(vec!["POST /api/inbox accepts valid payload".into()]);
    assert!(
        b.transition_to(Role ::Test ,Status ::Verified ).is_err(),
        "CXA-F024: a bug with an uncovered acceptance criterion must not reach VERIFIED"
    );
}

/// AC#3 -- even when SOME criteria pass, any remaining NOT TESTED criterion
/// still blocks VERIFIED (partial coverage is not enough).
#[test]
fn ac3_partial_coverage_with_one_uncovered_still_blocks() {
    let mut b = fixed_bug("BUG-F024-2");
    b.set_acceptance_criteria(vec![
        "request is accepted".into(),
        "response body echoes the id".into(),
        "malformed payload is rejected with 400".into(),
    ]);
    mark_all_passed(&mut b);
    let acs = b.acceptance_criteria().to_vec();
    assert!(b.set_test_case_result(&acs[2], false, None, None, "t".into()));
    assert!(
        b.transition_to(Role ::Test ,Status ::Verified ).is_err(),
        "CXA-F024: an uncovered criterion keeps the ticket out of VERIFIED"
    );
}

/// AC#3 (green guard) -- a bug whose every criterion is covered by a passing
/// test case must be allowed to reach VERIFIED.
#[test]
fn ac3_all_covered_is_not_blocked() {
    let mut b = fixed_bug("BUG-F024-3");
    b.set_acceptance_criteria(vec!["request is accepted".into()]);
    mark_all_passed(&mut b);
    assert!(
        b.transition_to(Role ::Test ,Status ::Verified ).is_ok(),
        "CXA-F024: all criteria covered -> VERIFIED allowed"
    );
}

/// AC#4 -- a ticket whose acceptance criteria were deliberately clarified to
/// empty (the BA clarifies-to-empty path) has nothing to cover, so it may still
/// transition to VERIFIED.
#[test]
fn ac4_clarified_to_empty_allows_transition() {
    let mut b = fixed_bug("BUG-F024-4");
    // The BA clarified every criterion down to nothing -> zero criteria remain.
    b.set_acceptance_criteria(vec![]);
    assert!(
        b.acceptance_criteria().is_empty(),
        "clarify-to-empty leaves no criteria to cover"
    );
    // Nothing left uncovered -> VERIFIED must be allowed under coverage gating.
    assert!(
        b.transition_to(Role ::Test ,Status ::Verified ).is_ok(),
        "CXA-F024: empty acceptance criteria (clarify-to-empty) must not block VERIFIED"
    );
}

/// AC#4 (matrix view) -- with zero criteria there is exactly one valid
/// rendering: no test cases to show in the Test Coverage tab.
#[test]
fn ac4_empty_matrix_shows_no_rows() {
    let mut b = fixed_bug("BUG-F024-5");
    b.set_acceptance_criteria(vec![]);
    assert!(b.test_cases().is_empty(), "no criteria -> no test cases to show");
}
