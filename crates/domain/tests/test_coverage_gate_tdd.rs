//! TDD tests for CXA-F024 — Test-to-AC Traceability Matrix (domain slice).
//!
//! These tests encode the EXACT acceptance criteria and fail until the
//! coverage invariant exists on the aggregate:
//!
//! - AC3: "Tickets with any NOT TESTED or PARTIALLY COVERED acceptance
//!   criteria are blocked from transitioning to VERIFIED (domain invariant
//!   enforced on transition_to(Status::Verified))."
//! - AC4: "acceptance criteria that are deliberately empty after
//!   clarification … allow the transition — the matrix shows no criteria to
//!   cover."
//! - AC5: "for non-UI tickets where evidence is request/response, the matrix
//!   marks the API call as covering the affected criteria rather than
//!   requiring a file-based test link."
//!
//! Coverage-status mapping over the types that exist: a criterion whose test
//! case is `Pending` is NOT TESTED, `Passed` is COVERED, `Failed` is
//! PARTIALLY COVERED (a test addresses it but does not demonstrate it).
//!
//! Pure functions over the domain aggregate — no IO, no engine, no store.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_domain::ticket::{TestCaseStatus, TicketType};
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId};

const AT: &str = "2026-08-20T01:00:00Z";

const AC_REGRESSION: &str = "the bug can no longer be reproduced on a fresh checkout";
const AC_TEST_SHIPPED: &str = "the fix ships with a regression test";

/// A bug that reached `Fixed` with the given acceptance criteria and one
/// synced test case per criterion — exactly the state TEST verification
/// starts from (`RunTestUseCase` keeps `test_cases` synced to criteria).
fn fixed_bug(acceptance_criteria: &[&str]) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new("BUG-F024").expect("valid id"),
        TicketType::Bug,
        "crash on save",
        "the app crashes when saving an empty record",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    t.set_acceptance_criteria(
        acceptance_criteria
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
    );
    t.ensure_test_cases_from_acceptance();
    t.claim(Role::DevBug, "dev@host", "2026-08-20T00:00:00Z")
        .expect("unclaimed bug is claimable");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("dev may mark a claimed bug fixed");
    t
}

/// Mark one criterion's test case passed, attaching the evidence note TEST
/// writes (no image — screenshots are attached later, UI tickets only).
fn pass_case(t: &mut Ticket, criterion: &str, note: &str) {
    assert!(
        t.set_test_case_result(criterion, true, Some(note.to_owned()), None, AT.to_owned()),
        "a synced test case must exist for the criterion"
    );
}

// -------------------------------------------------------------------------
// AC3 — blocked on NOT TESTED / PARTIALLY COVERED
// -------------------------------------------------------------------------

#[test]
fn ac3_verified_is_blocked_while_a_criterion_is_not_tested() {
    // Both cases still Pending == NOT TESTED: Verified must be rejected by
    // the aggregate and the ticket must stay Fixed.
    let mut t = fixed_bug(&[AC_REGRESSION, AC_TEST_SHIPPED]);
    let verdict = t.transition_to(Role::Test, Status::Verified);
    assert!(
        verdict.is_err(),
        "NOT TESTED criteria must block Verified, got {verdict:?}"
    );
    assert_eq!(t.status(), Status::Fixed, "the ticket stays Fixed");
}

#[test]
fn ac3_verified_is_blocked_when_coverage_is_only_partial() {
    // One criterion Passed, the other still Pending == PARTIALLY COVERED:
    // still blocked.
    let mut t = fixed_bug(&[AC_REGRESSION, AC_TEST_SHIPPED]);
    pass_case(&mut t, AC_REGRESSION, "repro gone on main");
    let verdict = t.transition_to(Role::Test, Status::Verified);
    assert!(
        verdict.is_err(),
        "PARTIALLY COVERED criteria must block Verified, got {verdict:?}"
    );
    assert_eq!(t.status(), Status::Fixed);
}

#[test]
fn ac3_verified_is_blocked_when_a_criterion_test_fails() {
    // A criterion whose test FAILED is at best PARTIALLY COVERED (the test
    // addresses it but does not demonstrate it): blocked like the others.
    let mut t = fixed_bug(&[AC_REGRESSION, AC_TEST_SHIPPED]);
    pass_case(&mut t, AC_REGRESSION, "repro gone on main");
    assert!(
        t.set_test_case_result(
            AC_TEST_SHIPPED,
            false,
            Some("no regression test in the diff".to_owned()),
            None,
            AT.to_owned(),
        ),
        "synced case must exist"
    );
    let verdict = t.transition_to(Role::Test, Status::Verified);
    assert!(
        verdict.is_err(),
        "a failed criterion must block Verified, got {verdict:?}"
    );
}

#[test]
fn ac3_human_verify_verdict_is_gated_the_same_way() {
    // The dashboard's verify button funnels through the same aggregate method
    // as `Role::User` (human_transition in the server) — the invariant must
    // hold on the human path too.
    let mut t = fixed_bug(&[AC_REGRESSION]);
    let verdict = t.transition_to(Role::User, Status::Verified);
    assert!(
        verdict.is_err(),
        "the human verify path is bound by the same coverage gate, got {verdict:?}"
    );
}

#[test]
fn ac3_fully_covered_ticket_still_reaches_verified() {
    // Control: every criterion Passed == COVERED — the gate must not
    // over-block a fully verified ticket.
    let mut t = fixed_bug(&[AC_REGRESSION, AC_TEST_SHIPPED]);
    pass_case(&mut t, AC_REGRESSION, "repro gone on main");
    pass_case(
        &mut t,
        AC_TEST_SHIPPED,
        "regression test fails on pre-fix code",
    );
    t.transition_to(Role::Test, Status::Verified)
        .expect("COVERED criteria must allow Verified");
    assert_eq!(t.status(), Status::Verified);
}

// -------------------------------------------------------------------------
// AC4 — deliberately empty criteria allow the transition
// -------------------------------------------------------------------------

#[test]
fn ac4_criteria_deliberately_empty_after_clarification_allow_verified() {
    // The BA clarified the ticket to deliberately-empty acceptance criteria:
    // there are no criteria to cover, the matrix is empty, and Verified is
    // allowed.
    let mut t = fixed_bug(&[AC_REGRESSION]);
    t.set_acceptance_criteria(Vec::new());
    t.ensure_test_cases_from_acceptance();
    assert!(t.acceptance_criteria().is_empty());
    assert!(
        t.test_cases().is_empty(),
        "no criteria to cover means no matrix rows"
    );
    t.transition_to(Role::Test, Status::Verified)
        .expect("deliberately empty criteria allow Verified");
    assert_eq!(t.status(), Status::Verified);
}

// -------------------------------------------------------------------------
// AC5 — request/response evidence covers a non-UI ticket
// -------------------------------------------------------------------------

#[test]
fn ac5_request_response_evidence_covers_a_non_ui_ticket() {
    // Non-UI ticket: the proof for the criterion is a captured
    // request/response (the case note), not a file-based screenshot link —
    // that must still count as COVERED.
    let mut t = fixed_bug(&["GET /api/health answers 200 ok"]);
    pass_case(
        &mut t,
        "GET /api/health answers 200 ok",
        "GET http://127.0.0.1:8101/api/health\nHTTP 200\n{\"ok\":true}",
    );
    let case = &t.test_cases()[0];
    assert_eq!(case.status, TestCaseStatus::Passed);
    assert!(
        case.evidence.as_ref().is_some_and(|e| e.image.is_none()),
        "request/response evidence carries no file-based image link"
    );
    t.transition_to(Role::Test, Status::Verified)
        .expect("API request/response evidence covers the criterion");
    assert_eq!(t.status(), Status::Verified);
}
