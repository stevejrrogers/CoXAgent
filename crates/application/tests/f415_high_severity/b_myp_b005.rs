// LOCKDOWN: CXA-MYP-B005 — duplicate request lines tolerate, never double-book.
use coxagent_application::selection::validate_committed_bug_ids;
use coxagent_domain::Priority;

#[test]
fn cxaf415_myp_b005_duplicate_commit_requests_are_tolerated_not_duplicated() {
    let board = vec![super::open_bug("F415-b5-a", Priority::High)];
    let none =
        validate_committed_bug_ids(&["F415-b5-a".to_owned(), "F415-b5-a".to_owned()], &board);
    assert!(
        none.is_empty(),
        "CXA-MYP-B005: duplicates are tolerated (single logical commitment), got {none:?}"
    );
}
