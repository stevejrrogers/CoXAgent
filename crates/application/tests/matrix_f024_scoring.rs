//! CXA-F024 — Test-to-AC traceability: scoring + matrix + the Verified gate,
//! exercised end to end through the real types (Ticket aggregate, TEST-agent
//! verdicts, the matching engine). Pure functions over real state — no
//! server, no harness, no IO.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::parsing::TestVerdict;
use coxagent_application::state::ProjectState;
use coxagent_application::use_cases::coverage::{match_score, matches_criterion, record_verdicts};
use coxagent_domain::coverage::CoverageStatus;
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};

const AC_REPRO: &str = "repro steps are listed on the ticket";
const AC_ROOT_CAUSE: &str = "root cause is fixed at source";

/// A bug driven to `Fixed` — the only lifecycle that reaches `Verified`.
fn fixed_bug(criteria: &[&str]) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new("BUG-CXA-F024").expect("id"),
        TicketType::Bug,
        "traceability fixture",
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

fn verdict(ac: &str, passed: bool, note: &str, tests: &[&str]) -> TestVerdict {
    TestVerdict {
        ac: ac.to_owned(),
        passed,
        note: note.to_owned(),
        route: String::new(),
        tests: tests.iter().map(|t| (*t).to_owned()).collect(),
    }
}

fn status_of(t: &Ticket, criterion: &str) -> CoverageStatus {
    match t
        .coverage_matrix()
        .into_iter()
        .find(|e| e.criterion == criterion)
    {
        Some(e) => e.status,
        None => panic!("no matrix row for {criterion}"),
    }
}

#[test]
fn exact_verdict_covers_its_criterion_and_names_the_test_file() {
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug(&[AC_REPRO, AC_ROOT_CAUSE]));
    let v = verdict(
        AC_REPRO,
        true,
        "gate test green",
        &["crates/domain/tests/gate.rs"],
    );

    assert!(record_verdicts(&mut state, &[v], "t1"));
    let t = &state.tickets[0];
    assert_eq!(status_of(t, AC_REPRO), CoverageStatus::Covered);
    assert_eq!(status_of(t, AC_ROOT_CAUSE), CoverageStatus::NotTested);
    assert_eq!(
        t.coverage_matrix()[0].sources,
        vec!["crates/domain/tests/gate.rs".to_owned()],
        "the matrix shows WHICH test file addresses the criterion"
    );
}

#[test]
fn paraphrased_verdict_is_fuzzy_matched_to_its_criterion() {
    // The TEST agent was asked for word-for-word text but paraphrased; the
    // keyword matcher must still land the verdict on the right criterion.
    assert!(
        matches_criterion(AC_REPRO, "repro steps listed on ticket"),
        "fixture sanity: the paraphrase clears the keyword/fuzzy bar"
    );
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug(&[AC_REPRO]));
    let v = verdict("repro steps listed on ticket", true, "", &[]);

    assert!(record_verdicts(&mut state, &[v], "t1"));
    assert_eq!(
        status_of(&state.tickets[0], AC_REPRO),
        CoverageStatus::Covered
    );
}

#[test]
fn unmatched_verdict_attaches_to_nothing() {
    assert!(!matches_criterion(
        AC_REPRO,
        "login returns 500 on bad input"
    ));
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug(&[AC_REPRO]));
    let v = verdict("login returns 500 on bad input", true, "", &[]);

    assert!(
        !record_verdicts(&mut state, &[v], "t1"),
        "nothing matched, nothing changed"
    );
    assert_eq!(
        status_of(&state.tickets[0], AC_REPRO),
        CoverageStatus::NotTested
    );
}

#[test]
fn failing_verdict_is_partially_covered_and_blocks_verified() {
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug(&[AC_REPRO]));
    let v = verdict(AC_REPRO, false, "repro missing", &[]);

    assert!(record_verdicts(&mut state, &[v], "t1"));
    assert_eq!(
        status_of(&state.tickets[0], AC_REPRO),
        CoverageStatus::PartiallyCovered
    );
    let mut t = state.tickets.remove(0);
    assert!(
        t.transition_to(Role::Test, Status::Verified).is_err(),
        "PARTIALLY COVERED blocks Verified exactly like NOT TESTED"
    );
    assert_eq!(t.status(), Status::Fixed);
}

#[test]
fn api_request_response_covers_non_ui_criterion_without_file_link() {
    // Non-UI ticket, evidence is the request/response itself: the matrix must
    // show the API call as the source — no file-based test link required.
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug(&[AC_ROOT_CAUSE]));
    let v = verdict(AC_ROOT_CAUSE, true, "GET /api/health 200", &[]);

    assert!(record_verdicts(&mut state, &[v], "t1"));
    let t = &state.tickets[0];
    assert_eq!(status_of(t, AC_ROOT_CAUSE), CoverageStatus::Covered);
    let sources = &t.coverage_matrix()[0].sources;
    assert_eq!(sources.as_slice(), &["GET /api/health 200".to_owned()][..]);
    assert!(
        !sources.iter().any(|s| s.contains(".rs")),
        "an API-evidence source is not a file link"
    );
    // And the criterion is genuinely covered — the gate lets it verify.
    let mut t = state.tickets.remove(0);
    t.transition_to(Role::Test, Status::Verified)
        .expect("API-evidence coverage satisfies the Verified gate");
}

#[test]
fn not_tested_criterion_blocks_verified_until_every_criterion_passes() {
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug(&[AC_REPRO, AC_ROOT_CAUSE]));
    let mut t = state.tickets.remove(0);

    assert!(
        t.transition_to(Role::Test, Status::Verified).is_err(),
        "a criterion with no verdict is NOT TESTED — the transition blocks"
    );
    assert_eq!(
        t.status(),
        Status::Fixed,
        "a blocked transition changes nothing"
    );

    assert!(t.set_test_case_result(AC_REPRO, true, None, None, "t2".into()));
    assert!(
        t.transition_to(Role::Test, Status::Verified).is_err(),
        "still blocked while the second criterion has no verdict"
    );
    assert!(t.set_test_case_result(AC_ROOT_CAUSE, true, None, None, "t3".into()));
    t.transition_to(Role::Test, Status::Verified)
        .expect("every criterion covered — the gate opens");
    assert_eq!(t.status(), Status::Verified);
}

#[test]
fn criteria_emptied_after_clarification_verify_with_an_empty_matrix() {
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug(&[AC_REPRO]));
    let mut t = state.tickets.remove(0);

    t.clarify(
        Role::Ba,
        "Requirement restated; criteria deliberately left empty pending re-proposal.",
    )
    .expect("BA may restate");
    t.set_acceptance_criteria(Vec::new());
    assert!(
        t.coverage_matrix().is_empty(),
        "no criteria — no rows to cover"
    );
    t.transition_to(Role::Test, Status::Verified)
        .expect("nothing to cover — the transition is allowed");
    assert_eq!(t.status(), Status::Verified);
}

#[test]
fn scoring_prefers_the_strongest_candidate() {
    let ac = "login returns 500 on bad input";
    let exact = "login returns 500 on bad input";
    let near = "login returns 500 for bad input";
    let far = "logout clears the session";
    assert!((match_score(ac, exact) - 1.0).abs() < 1e-9);
    assert!(match_score(ac, near) > match_score(ac, far));
    assert!(
        matches_criterion(ac, near),
        "one-word edit stays within Levenshtein 0.3"
    );
    assert!(!matches_criterion(ac, far));
}
