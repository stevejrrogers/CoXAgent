//! Test-to-AC traceability (CXA-F024) — the coverage matrix linking each
//! acceptance criterion to the evidence that demonstrates it.
//!
//! Pure domain: statuses and entries are computed from the ticket's own
//! `acceptance_criteria` + `test_cases`; no IO, no matching heuristics here.
//! (Fuzzy verdict-to-criterion matching lives in the application layer, which
//! writes its outcome back through `Ticket::set_test_case_sources`.)

use crate::ticket::TestCaseStatus;
use crate::ticket::{TestCase, Ticket};
use serde::{Deserialize, Serialize};

/// Coverage verdict for ONE acceptance criterion. Every status other than
/// [`CoverageStatus::Covered`] blocks the `Fixed -> Verified` transition —
/// the matrix's whole reason to exist is making that gap visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// A test case for this criterion PASSED — the criterion is demonstrated.
    Covered,
    /// The criterion was exercised but not demonstrated: its case FAILED.
    PartiallyCovered,
    /// No test case exists for the criterion, or its verdict is still pending.
    NotTested,
}

/// One row of the traceability matrix: an acceptance criterion, how well it is
/// covered, and where the proof lives (test files, or an API request/response
/// for non-UI tickets — a file link is not required, evidence is).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageEntry {
    /// The acceptance-criterion text (verbatim from the ticket).
    pub criterion: String,
    pub status: CoverageStatus,
    /// What demonstrates this criterion — relative test-file paths, or an
    /// API request/response line. Empty means "verdict only, no linked proof".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

/// Map a test case to its coverage status. `None` (no case at all) and a
/// pending case both read as NOT TESTED; a failed case is PARTIALLY COVERED
/// (the criterion was reached, the demonstration failed); only a pass covers.
#[must_use]
pub fn status_of_case(case: Option<&TestCase>) -> CoverageStatus {
    match case.map(|c| c.status) {
        Some(TestCaseStatus::Passed) => CoverageStatus::Covered,
        Some(TestCaseStatus::Failed) => CoverageStatus::PartiallyCovered,
        Some(TestCaseStatus::Pending) | None => CoverageStatus::NotTested,
    }
}

/// The acceptance criteria NOT demonstrated by a passing test case, in ticket
/// order. Empty criteria list (deliberately emptied after clarification, or
/// never set) yields an empty result — nothing to cover, nothing to block.
#[must_use]
pub fn uncovered(criteria: &[String], cases: &[TestCase]) -> Vec<String> {
    criteria
        .iter()
        .filter(|ac| {
            cases
                .iter()
                .find(|c| &c.description == *ac)
                .map(|c| c.status)
                != Some(TestCaseStatus::Passed)
        })
        .cloned()
        .collect()
}

/// The full traceability matrix for a ticket: one row per acceptance
/// criterion, in ticket order. Criteria with no test case (desynced state)
/// surface as NOT TESTED rather than disappearing.
#[must_use]
pub fn matrix_for(ticket: &Ticket) -> Vec<CoverageEntry> {
    let cases = ticket.test_cases();
    ticket
        .acceptance_criteria()
        .iter()
        .map(|ac| {
            let case = cases.iter().find(|c| &c.description == ac);
            CoverageEntry {
                criterion: ac.clone(),
                status: status_of_case(case),
                sources: case.map(|c| c.sources.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TicketId;
    use crate::kinds::{Complexity, Priority, TicketType};
    use crate::ticket::TestCase;

    /// Build a ticket through the public aggregate API only: criteria, synced
    /// cases, then the requested pass/fail verdicts. `cases` texts must be
    /// criteria texts (the aggregate has no other way to attach a verdict).
    fn ticket_with(criteria: &[&str], verdicts: &[(&str, bool)]) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new("BUG-COV").expect("id"),
            TicketType::Bug,
            "coverage fixture",
            "fixture",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("ticket");
        t.set_acceptance_criteria(criteria.iter().map(|c| (*c).to_owned()).collect());
        t.ensure_test_cases_from_acceptance();
        for (d, passed) in verdicts {
            assert!(t.set_test_case_result(d, *passed, None, None, "t0".into()));
        }
        t
    }

    fn case(status: TestCaseStatus) -> TestCase {
        TestCase {
            description: "ac".into(),
            status,
            evidence: None,
            sources: Vec::new(),
            history: Vec::new(),
        }
    }

    #[test]
    fn statuses_map_from_case_verdicts() {
        assert_eq!(
            status_of_case(Some(&case(TestCaseStatus::Passed))),
            CoverageStatus::Covered
        );
        assert_eq!(
            status_of_case(Some(&case(TestCaseStatus::Failed))),
            CoverageStatus::PartiallyCovered
        );
        assert_eq!(
            status_of_case(Some(&case(TestCaseStatus::Pending))),
            CoverageStatus::NotTested
        );
        assert_eq!(status_of_case(None), CoverageStatus::NotTested);
    }

    #[test]
    fn uncovered_reports_criteria_without_a_passing_case() {
        let t = ticket_with(
            &["ac one", "ac two"],
            &[("ac one", true), ("ac two", false)],
        );
        let u = uncovered(t.acceptance_criteria(), t.test_cases());
        assert_eq!(u, vec!["ac two".to_owned()]);
    }

    #[test]
    fn empty_criteria_have_nothing_to_cover() {
        let t = ticket_with(&[], &[]);
        assert!(uncovered(t.acceptance_criteria(), t.test_cases()).is_empty());
        assert!(matrix_for(&t).is_empty());
    }

    #[test]
    fn desynced_case_leaves_criterion_not_tested_with_no_sources() {
        // Criteria edited AFTER a case passed: the old case no longer matches
        // any criterion, so the new criterion is honestly NOT TESTED.
        let mut t = ticket_with(&["ac one"], &[("ac one", true)]);
        t.set_acceptance_criteria(vec!["reworded criterion".to_owned()]);
        let m = matrix_for(&t);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].criterion, "reworded criterion");
        assert_eq!(m[0].status, CoverageStatus::NotTested);
        assert!(m[0].sources.is_empty());
    }

    #[test]
    fn matrix_carries_sources_from_the_case() {
        let mut t = ticket_with(&["ac one"], &[("ac one", true)]);
        assert!(t.set_test_case_sources("ac one", vec!["crates/x/tests/a.rs".into()]));
        let m = matrix_for(&t);
        assert_eq!(m[0].status, CoverageStatus::Covered);
        assert_eq!(m[0].sources, vec!["crates/x/tests/a.rs".to_owned()]);
    }
}
