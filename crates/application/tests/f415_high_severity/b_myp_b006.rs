// LOCKDOWN: CXA-MYP-B006 — vanished-commit accounting in the targeted burn.
//! A committed bug id that no longer resolves to a ticket on the board
//! (evicted/deleted) must not be counted as fixed; while ANY open named commit
//! survives, `burn_exit` stays `StillBurning` and `burn_exit_why` says
//! "vanished" honestly instead of declaring victory over ghosts. Diagnosis:
//! docs/bug-burn-f415-triage.md#myp-b006.
use coxagent_application::selection::{burn_exit, burn_exit_why, BurnExit};
use coxagent_domain::Priority;

#[test]
fn cxaf415_myp_b006_burn_exit_counts_a_vanished_commit_as_outstanding() {
    let now = 1_800_000_000_i64;
    // Deadline in the future: the ONLY early exit is every surviving commit
    // being Fixed/Verified. One commit still exists and is Open; one was
    // evicted from the board entirely (only its name remains).
    let state = super::burn_board(
        &["F415-b6-open", "F415-b6-gone"],
        now + 3_600,
        vec![super::open_bug("F415-b6-open", Priority::High)],
    );

    match burn_exit(&state, now) {
        BurnExit::AllCommittedFixed => panic!(
            "CXA-MYP-B006: a vanished commit counts as neither fixed nor complete —              one OPEN named bug remains, yet the exit claimed all-fixed"
        ),
        BurnExit::DeadlineElapsed { remaining } => panic!(
            "CXA-MYP-B006: deadline has NOT passed — premature timebox exit with remaining={remaining}"
        ),
        BurnExit::StillBurning { fixed_count, .. } => {
            assert_eq!(fixed_count, 0, "the vanished commit must not count toward fixed");
        }
    }

    let why = burn_exit_why(&state, now);
    assert!(
        why.contains("vanished"),
        "CXA-MYP-B006: the banner must attribute WHY the burn continues despite the          eviction (got: {why:?}) — silent idling of an armed pause is this bug class"
    );
}
