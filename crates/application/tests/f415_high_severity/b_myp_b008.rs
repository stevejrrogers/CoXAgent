// LOCKDOWN: CXA-MYP-B008 — completion invariant keyed on real Fixed/Verified.
use coxagent_application::selection::{burn_exit, BurnExit};
use coxagent_domain::{Priority, Role, Status};

#[test]
fn cxaf415_myp_b008_targeted_burn_exits_when_every_commit_is_fixed() {
    let now = 1_800_000_000_i64;
    let mut fixed = super::open_bug("F415-b8-fixed", Priority::High);
    fixed
        .transition_to(Role::DevBug, Status::InProgress)
        .expect("claim");
    fixed
        .transition_to(Role::DevBug, Status::Fixed)
        .expect("fix");
    let state = super::burn_board(&["F415-b8-fixed"], now + 3_600, vec![fixed]);
    assert_eq!(
        burn_exit(&state, now),
        BurnExit::AllCommittedFixed,
        "CXA-MYP-B008: a fully Fixed committed set inside the window exits successfully"
    );
}
