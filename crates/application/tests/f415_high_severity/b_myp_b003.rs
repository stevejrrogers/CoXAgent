// LOCKDOWN: CXA-MYP-B003 — only open/in-progress bugs join the commit set.
use coxagent_application::selection::validate_committed_bug_ids;
use coxagent_domain::Priority;

#[test]
fn cxaf415_myp_b003_burn_admission_requires_open_inprogress_status() {
    let mut done = super::open_bug("F415-b3-done", Priority::High);
    done.transition_to(
        coxagent_domain::Role::DevBug,
        coxagent_domain::Status::InProgress,
    )
    .expect("claim");
    done.transition_to(
        coxagent_domain::Role::DevBug,
        coxagent_domain::Status::Fixed,
    )
    .expect("fix");
    let rejections = validate_committed_bug_ids(&["F415-b3-done".to_owned()], &[done]);
    assert!(
        !rejections.is_empty(),
        "CXA-MYP-B003: a terminal-state bug must not re-enter the committed burn set"
    );
}
