// LOCKDOWN: CXA-MYP-B004 — severity-1/2 burns commit HIGH bugs only.
use coxagent_application::selection::validate_committed_bug_ids;
use coxagent_domain::Priority;

#[test]
fn cxaf415_myp_b004_burn_admission_commits_only_high_priority() {
    let low = super::open_bug("F415-b4-low", Priority::Low);
    let rejections = validate_committed_bug_ids(&["F415-b4-low".to_owned()], &[low]);
    assert!(
        rejections.iter().any(|r| r.reason.contains("high")),
        "CXA-MYP-B004: non-high bugs must be refused with a reason naming the priority rule"
    );
}
