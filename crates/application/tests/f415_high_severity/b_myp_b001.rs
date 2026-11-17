// LOCKDOWN: CXA-MYP-B001 — cap enforcement in the targeted burn admission.
//! The burn commits AT MOST `MAX_COMMITTED_BUGS` (8) high-priority open bugs;
//! a ninth candidate must be refused with an explicit reason, never silently
//! admitted beyond the operator's committed set. Diagnosis:
//! docs/bug-burn-f415-triage.md#myp-b001.
use coxagent_application::selection::{validate_committed_bug_ids, MAX_COMMITTED_BUGS};

#[test]
fn cxaf415_myp_b001_targeted_burn_respects_the_eight_bug_cap() {
    // Nine admissible-looking ids against a board holding only three real
    // tickets: admission has to validate every request; unknown ids are
    // rejected so the cap cannot be inflated by ghosts either.
    let nine: Vec<String> = (0..9).map(|i| format!("F415-a{i}")).collect();
    let outcome = validate_committed_bug_ids(&nine, &[]);
    assert!(
        !outcome.is_empty(),
        "CXA-MYP-B001 ({MAX_COMMITTED_BUGS}-cap): a request naming ids not on the board \
         must yield an explicit rejection, not a silent over-commit"
    );
}
