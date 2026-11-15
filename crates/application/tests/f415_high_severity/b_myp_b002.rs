// LOCKDOWN: CXA-MYP-B002 — admission rejects ids that are not real tickets.
use coxagent_application::selection::validate_committed_bug_ids;
use coxagent_domain::Priority;

#[test]
fn cxaf415_myp_b002_burn_admission_rejects_unknown_ticket_ids() {
    let board = vec![super::open_bug("F415-b2-real", Priority::High)];
    let rejections = validate_committed_bug_ids(&["F415-b2-ghost".to_owned()], &board);
    assert!(
        rejections.iter().any(|r| r.id == "F415-b2-ghost"),
        "CXA-MYP-B002: committing an id absent from the board must yield an explicit rejection"
    );
}
