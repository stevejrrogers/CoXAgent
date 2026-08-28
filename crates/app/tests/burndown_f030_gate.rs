//! CXA-F030 acceptance gate -- "Bug burn-down sprint before next feature work".
//!
//! Written before implementation (TDD) so the acceptance criteria are pinned
//! as executable invariants over the state/domain types; compiles today and
//! fails only for the missing behaviour. The scope here is SPRINT-scoped,
//! unlike F022 (the four bugs gating F001) and F032 (the whole bug backlog):
//! AC#1 selects the burn-down scope at sprint start -- every open bug that
//! BLOCKS the next feature work (the feature depends on it, `depends_on`) or
//! PRECEDES it (earlier backlog position) -- and the selection must be
//! visible in the sprint status (`Sprint::committed`).
//!
//! Red today, and why (two production paths, driven through existing APIs):
//!   * AC#1 -- `sprint::advance` opens sprint 1 via the capacity-based
//!     auto-commit (`open_backlog`), which reserves a seat for a ready
//!     feature and caps the open bugs at the remaining capacity (6 - 1 = 5):
//!     with six open bugs, one is left UNCOMMITTED. A burn-down sprint must
//!     commit every in-scope open bug, whatever the capacity math says.
//!   * AC#5 -- `selection::next_ready_feature` gates a feature on
//!     `deps_satisfied`, which accepts only `Done | Documented` dependents:
//!     even with every blocking bug `Verified`, the next feature work stays
//!     unselectable. A clean burn-down baseline must release it.
//!
//! AC#2/#3/#4 are pinned as pure invariants over the legal transition table
//! and the evidence store (the shape the F022/F032 gates used); they pass
//! today and guard the semantics the implementation must keep. AC#3's
//! production TEST path already records compliant evidence (landed with
//! F022) and stays covered by `burndown_f022_gate.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::selection;
use coxagent_application::sprint::{self, SprintPolicy};
use coxagent_application::state::ProjectState;
use coxagent_domain::{
    Complexity, DomainError, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};

const REGRESSION_EVIDENCE_LABEL: &str = "REGRESSION TEST";
/// AC#3: the detail must contain a PASS marker plus proof of a clean
/// reproduction and a root cause, and must exclude masking talk.
const PASS_MARKER: &str = "PASS";
const CLEAN_REPRO_MARKER: &str = "reproduces";
const ROOT_CAUSE_MARKER: &str = "root cause";

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

/// A backlog bug: bugs start `Open` by domain law (`Ticket::new`).
fn bug(id: &str) -> Ticket {
    Ticket::new(
        tid(id),
        TicketType::Bug,
        format!("defect {id}"),
        "repro: run the flow, observe the failure",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("bug")
}

/// A designed, ready feature (the DoR re-check demands the technical design).
fn ready_feature(id: &str) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Feature,
        format!("feature {id}"),
        "",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("feature");
    t.set_technical_design(Role::Sa, TechnicalDesign::default())
        .expect("design");
    t.transition_to(Role::Sa, Status::Ready).expect("ready");
    t
}

/// AC#1: the next feature work -- the feature the burn-down sprint is clearing
/// the road for. The first still-actionable (Pending/Ready) feature in the
/// backlog; every fixture holds exactly one, so the choice is unambiguous.
fn next_feature_work(state: &ProjectState) -> Option<TicketId> {
    state
        .tickets
        .iter()
        .find(|t| {
            matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
                && matches!(t.status(), Status::Pending | Status::Ready)
        })
        .map(|t| t.id().clone())
}

fn backlog_position(state: &ProjectState, id: &TicketId) -> Option<usize> {
    state.tickets.iter().position(|t| t.id() == id)
}

/// AC#1's burn-down scope: every OPEN bug that blocks the next feature work
/// (the feature's `depends_on` names it) or precedes it in the backlog.
fn burn_down_scope(state: &ProjectState) -> Vec<TicketId> {
    let Some(feature) = next_feature_work(state) else {
        return Vec::new();
    };
    let feature_pos = backlog_position(state, &feature);
    state
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
        .filter(|t| {
            let blocks = state
                .ticket(&feature)
                .is_some_and(|f| f.depends_on().contains(t.id()));
            let precedes = matches!(
                (backlog_position(state, t.id()), feature_pos),
                (Some(bp), Some(fp)) if bp < fp
            );
            blocks || precedes
        })
        .map(|t| t.id().clone())
        .collect()
}

/// AC#3: does this bug carry its OWN recorded QA evidence labelled
/// "REGRESSION TEST" whose detail contains the PASS marker plus proof of a
/// clean reproduction and a root cause, with none of the masking talk?
fn has_regression_pass_evidence(state: &ProjectState, id: &TicketId) -> bool {
    state
        .ticket_evidence
        .get(&id.to_string())
        .is_some_and(|evs| {
            evs.iter().any(|e| {
                e.label.starts_with(REGRESSION_EVIDENCE_LABEL)
                    && e.detail.contains(PASS_MARKER)
                    && e.detail.contains(CLEAN_REPRO_MARKER)
                    && e.detail.contains(ROOT_CAUSE_MARKER)
                    && !e.detail.contains("symptom")
                    && !e.detail.contains("workaround")
                    && !e.detail.contains("incidentally")
            })
        })
}

/// AC#4: a re-open exists when an Open/Fixed bug shares its title with an
/// already-cleared (Verified) in-scope bug. Closure can never be declared
/// while one exists -- the blocker has resurfaced.
fn reopened_copy_exists(state: &ProjectState, scope: &[TicketId]) -> bool {
    let cleared_titles: Vec<String> = scope
        .iter()
        .filter_map(|id| state.ticket(id))
        .filter(|t| matches!(t.status(), Status::Verified))
        .map(|t| t.title().to_string())
        .collect();
    state.tickets.iter().any(|t| {
        t.ticket_type() == TicketType::Bug
            && matches!(t.status(), Status::Open | Status::Fixed)
            && cleared_titles.contains(&t.title().to_string())
    })
}

/// AC#2/#3/#4: the burn-down is done only when EVERY in-scope bug has reached
/// `Status::Verified` (none left Open or Fixed), each carries its own
/// REGRESSION TEST PASS evidence, and no re-opened copy blocks closure.
/// Anything less is the burn-down reported as in progress.
fn burn_down_complete(state: &ProjectState, scope: &[TicketId]) -> bool {
    scope.iter().all(|id| {
        state
            .ticket(id)
            .is_some_and(|t| matches!(t.status(), Status::Verified))
            && has_regression_pass_evidence(state, id)
    }) && !reopened_copy_exists(state, scope)
}

/// Record the bug's own passing root-cause regression test as QA evidence --
/// the same shape of detail the production TEST path records on
/// `Fixed -> Verified` since F022.
fn record_regression_pass(state: &mut ProjectState, id: &TicketId) {
    state.add_evidence(
        &id.to_string(),
        "test",
        REGRESSION_EVIDENCE_LABEL,
        "PASS on current master; regression test fails on pre-fix code and \
         reproduces cleanly; root cause fixed at source.",
    );
}

/// Drive one bug along the ONLY legal route to `Fixed` (DEV-BUG's edges).
fn drive_to_fixed(state: &mut ProjectState, id: &TicketId) {
    {
        let t = state.ticket_mut(id).expect("ticket present");
        t.transition_to(Role::DevBug, Status::InProgress)
            .expect("Open -> InProgress is a legal bug edge for DEV-BUG");
    }
    {
        let t = state.ticket_mut(id).expect("ticket present");
        t.transition_to(Role::DevBug, Status::Fixed)
            .expect("InProgress -> Fixed is a legal bug edge for DEV-BUG");
    }
}

/// Drive one bug along the full legal route Open -> InProgress -> Fixed ->
/// Verified, recording its own regression PASS evidence on the way -- exactly
/// what burning one bug down must do.
fn clear_bug(state: &mut ProjectState, id: &TicketId) {
    drive_to_fixed(state, id);
    record_regression_pass(state, id);
    let t = state.ticket_mut(id).expect("ticket present");
    t.transition_to(Role::Test, Status::Verified)
        .expect("Fixed -> Verified is a legal bug edge for TEST");
}

/// The F030 starting line, reached through legal domain transitions only, and
/// then through the REAL sprint-start path (`sprint::advance`): one already
/// shipped feature (F200, Documented), the next feature work (F300, Ready)
/// blocked by six open bugs (the F030 ticket's own "clear all 6 open bugs"),
/// and the six open bugs B301..B306 each sitting earlier in the backlog than
/// F300 -- every one of them blocks-or-precedes the next feature work.
fn sprint_start_state() -> ProjectState {
    let mut s = ProjectState::default();
    // A previously shipped feature: the burn-down must not regress it (AC#5).
    let mut shipped = ready_feature("F200");
    shipped
        .transition_to(Role::DevFeature, Status::InProgress)
        .expect("claim");
    shipped
        .transition_to(Role::DevFeature, Status::Done)
        .expect("done");
    shipped
        .transition_to(Role::Docs, Status::Documented)
        .expect("documented");
    s.tickets.push(shipped);
    s.add_evidence(
        "F200",
        "test",
        "ship smoke",
        "release smoke PASS on the shipped build.",
    );
    // Six open bugs, backlog order ahead of the next feature work.
    for i in 1..=6 {
        s.tickets.push(bug(&format!("B30{i}")));
    }
    // The next feature work: blocked by all six bugs -- they gate it, which is
    // why the burn-down sprint must clear them before F300 can start.
    let mut next = ready_feature("F300");
    for i in 1..=6 {
        next.add_dependency(Role::Sa, tid(&format!("B30{i}")))
            .expect("link");
    }
    s.tickets.push(next);
    // Sprint start: the cycle opens the sprint that must carry the burn-down.
    let opened = sprint::advance(&mut s, 1, SprintPolicy::Cycles(10));
    assert_eq!(opened, Some(1), "sprint start opens the first sprint");
    s
}

// --- AC#1: at sprint start, every open bug that blocks or precedes the next
//     feature work is selected into the burn-down scope; no such bug is left
//     uncommitted, and the selection is visible in the sprint status. ---
//
// RED: `open_backlog` reserves a ready-feature seat and capacity-caps the
// open bugs, so with six bugs one is left out of `Sprint::committed`.
#[test]
fn ac1_sprint_start_commits_every_in_scope_open_bug_visible_in_sprint_status() {
    let s = sprint_start_state();
    let scope = burn_down_scope(&s);
    assert_eq!(
        scope.len(),
        6,
        "fixture: all six open bugs block or precede the next feature work"
    );
    let committed = &s
        .sprint
        .as_ref()
        .expect("sprint start opened the burn-down sprint")
        .committed;
    for id in &scope {
        assert!(
            committed.contains(id),
            "AC#1: open bug {id} blocks-or-precedes the next feature work and must be \
             selected into the burn-down scope, visible in the sprint status -- the \
             capacity-capped auto-commit leaves it uncommitted today"
        );
    }
}

// --- AC#2: clearing a strict subset (even one) leaves the burn-down reported
//     as in progress, not done. ---
#[test]
fn ac2_clearing_a_strict_subset_leaves_the_burn_down_in_progress_not_done() {
    let mut s = sprint_start_state();
    let scope = burn_down_scope(&s);
    clear_bug(&mut s, &scope[0]);
    assert!(
        !burn_down_complete(&s, &scope),
        "AC#2: one of six cleared -- the burn-down is still in progress, not done"
    );
}

// --- AC#2: the burn-down completes only when every in-scope bug has reached
//     Status::Verified with none left Open or Fixed. "Reached" goes through
//     the legal transition table; the domain refuses any shortcut. ---
#[test]
fn ac2_complete_only_when_every_in_scope_bug_is_verified_none_open_or_fixed() {
    let mut s = sprint_start_state();
    let scope = burn_down_scope(&s);
    // Every bug driven to Fixed, none Verified: "none left Open or Fixed" --
    // a Fixed-but-unverified bug keeps the burn-down from completing.
    for id in &scope {
        drive_to_fixed(&mut s, id);
        record_regression_pass(&mut s, id);
    }
    assert!(
        !burn_down_complete(&s, &scope),
        "AC#2: bugs parked at Fixed (never Verified) are not burned down"
    );
    // TEST confirms each fix: Fixed -> Verified, the only legal route there.
    for id in &scope {
        let t = s.ticket_mut(id).expect("ticket present");
        t.transition_to(Role::Test, Status::Verified)
            .expect("Fixed -> Verified is a legal bug edge for TEST");
    }
    assert!(
        burn_down_complete(&s, &scope),
        "AC#2: every in-scope bug Verified -> the burn-down is done"
    );
    for id in &scope {
        let t = s.ticket(id).expect("ticket present");
        assert_eq!(t.status(), Status::Verified);
        assert!(!matches!(t.status(), Status::Open | Status::Fixed));
    }
    // "reached Status::Verified" -- the burn-down path is the table's path:
    // jumping straight from Open to Verified is not a legal edge.
    let mut fresh = sprint_start_state();
    let t = fresh.ticket_mut(&tid("B301")).expect("ticket present");
    assert!(
        matches!(
            t.transition_to(Role::Test, Status::Verified),
            Err(DomainError::InvalidTransition { .. })
        ),
        "Open -> Verified is not a legal edge; the burn-down path is enforced"
    );
}

// --- AC#3: a Verified in-scope bug without its own recorded REGRESSION TEST
//     PASS evidence is NOT cleared -- recording the evidence completes it. ---
#[test]
fn ac3_verified_in_scope_bug_without_its_own_regression_evidence_is_not_cleared() {
    let mut s = sprint_start_state();
    let scope = burn_down_scope(&s);
    for id in &scope {
        drive_to_fixed(&mut s, id);
        let t = s.ticket_mut(id).expect("ticket present");
        t.transition_to(Role::Test, Status::Verified)
            .expect("Fixed -> Verified is a legal bug edge for TEST");
    }
    assert!(
        !burn_down_complete(&s, &scope),
        "AC#3: a Verified bug without its own regression PASS evidence is not cleared"
    );
    for id in &scope {
        record_regression_pass(&mut s, id);
    }
    assert!(
        burn_down_complete(&s, &scope),
        "AC#3: with each bug's own evidence recorded, the same state completes"
    );
}

// --- AC#3: EACH Verified in-scope bug carries its own recorded evidence --
//     one bug's proof never covers its siblings. ---
#[test]
fn ac3_each_in_scope_bug_needs_its_own_recorded_evidence() {
    let mut s = sprint_start_state();
    let scope = burn_down_scope(&s);
    for id in &scope {
        clear_bug(&mut s, id);
    }
    // Strip five of the six: B301's evidence must not vouch for the rest.
    for id in ["B302", "B303", "B304", "B305", "B306"] {
        s.ticket_evidence.insert(id.to_owned(), vec![]);
    }
    assert!(
        !burn_down_complete(&s, &scope),
        "AC#3: a sibling's evidence does not clear an evidence-less bug"
    );
    for id in ["B302", "B303", "B304", "B305", "B306"] {
        record_regression_pass(&mut s, &tid(id));
    }
    assert!(
        burn_down_complete(&s, &scope),
        "AC#3: each bug's OWN evidence completes it"
    );
}

// --- AC#3: evidence whose detail is masking talk -- symptom / workaround /
//     incidentally -- never counts, even when proof markers are present; and
//     the label must start with "REGRESSION TEST". A fix masked at symptom
//     level never counts as burned down. ---
#[test]
fn ac3_symptom_masked_fix_never_counts_as_burned_down() {
    let masking_details = [
        // Workaround talk only -- no proof at all.
        "workaround applied; symptom gone for now",
        // A PASS marker grounded in masking, not in a clean reproduction.
        "PASS observed; workaround applied and the symptom disappeared",
        // Incidental observation only.
        "noticed incidentally during a re-run; did not investigate",
        // All the proof markers, but the detail still talks masking: the
        // exclusion clause rejects it regardless.
        "PASS on master; reproduces; root cause patched, the symptom was \
         worked around incidentally",
    ];
    for (i, detail) in masking_details.iter().enumerate() {
        let id = format!("B30{}", i + 1);
        let mut s = sprint_start_state();
        let scope = burn_down_scope(&s);
        clear_bug(&mut s, &tid(&id));
        // Replace the legitimate evidence with the masking-only variant.
        s.ticket_evidence.insert(id.clone(), vec![]);
        s.add_evidence(&id, "test", REGRESSION_EVIDENCE_LABEL, detail);
        assert!(
            !burn_down_complete(&s, &scope),
            "AC#3: masking-only evidence must not count as burned down: {detail:?}"
        );
    }
    // The label matters too: the same proof under a different label is not
    // the required QA evidence labelled "REGRESSION TEST".
    let mut s = sprint_start_state();
    let scope = burn_down_scope(&s);
    clear_bug(&mut s, &tid("B301"));
    s.ticket_evidence.insert("B301".to_owned(), vec![]);
    s.add_evidence(
        "B301",
        "test",
        "QA note",
        "PASS on current master; reproduces cleanly; root cause fixed at source.",
    );
    assert!(
        !burn_down_complete(&s, &scope),
        "AC#3: proof under the wrong label is not the required REGRESSION TEST evidence"
    );
}

// --- AC#4: a re-opened copy of an already-cleared bug (same title, still
//     Open/Fixed) blocks closure regardless of how many originals are
//     Verified -- six Verified originals do not close the sprint while a
//     blocker has resurfaced. ---
#[test]
fn ac4_reopened_copy_blocks_closure_while_originals_are_verified() {
    let mut s = sprint_start_state();
    let scope = burn_down_scope(&s);
    for id in &scope {
        clear_bug(&mut s, id);
    }
    assert!(
        burn_down_complete(&s, &scope),
        "baseline: all six in-scope bugs cleared with their own evidence"
    );
    // The cleared B301's defect resurfaces: a NEW bug ticket sharing its
    // title, back in Open.
    let copy = Ticket::new(
        tid("B399"),
        TicketType::Bug,
        "defect B301".to_owned(),
        "re-opened copy of a cleared burn-down bug",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("copy");
    s.tickets.push(copy);
    assert!(
        !burn_down_complete(&s, &scope),
        "AC#4: a re-opened copy blocks closure regardless of six Verified originals"
    );
    // Fixed-but-unverified also blocks: the copy is "still Open/Fixed".
    drive_to_fixed(&mut s, &tid("B399"));
    assert!(
        !burn_down_complete(&s, &scope),
        "AC#4: the resurfaced blocker at Fixed still blocks closure"
    );
    // Burning the re-open down -- legal transitions plus its own evidence --
    // is what unblocks closure.
    record_regression_pass(&mut s, &tid("B399"));
    let t = s.ticket_mut(&tid("B399")).expect("ticket present");
    t.transition_to(Role::Test, Status::Verified)
        .expect("Fixed -> Verified is a legal bug edge for TEST");
    assert!(
        burn_down_complete(&s, &scope),
        "AC#4: closure returns once the resurfaced blocker is itself verified"
    );
}

// --- AC#5: when all in-scope bugs are Verified with regression evidence and
//     no re-opened copy exists, the next feature work can be selected and
//     started from a clean baseline, and no previously shipped behaviour
//     regressed by any of the fixes. ---
//
// RED: `selection::next_ready_feature` skips F300 because `deps_satisfied`
// only accepts `Done | Documented` dependents -- six Verified bugs never
// release it, so the clean baseline cannot hand DEV its next feature work.
#[test]
fn ac5_clean_baseline_selects_and_starts_next_feature_work_without_regressions() {
    let mut s = sprint_start_state();
    let scope = burn_down_scope(&s);
    for id in &scope {
        clear_bug(&mut s, id);
    }
    assert!(
        burn_down_complete(&s, &scope),
        "precondition: the burn-down is complete -- every in-scope bug Verified \
         with regression evidence, no re-opened copy"
    );
    // The next feature work is selectable again from the clean baseline...
    let next = selection::next_ready_feature(&s).expect(
        "AC#5: with every blocking bug Verified, F300 must be selectable -- \
         today deps_satisfied ignores Verified and the feature stays blocked",
    );
    assert_eq!(next, tid("F300"));
    // ...and DEV can start it.
    let t = s.ticket_mut(&next).expect("ticket present");
    t.transition_to(Role::DevFeature, Status::InProgress)
        .expect("Ready -> InProgress is a legal feature edge for DEV-FEATURE");
    // ...and no previously shipped behaviour regressed: the shipped feature's
    // status and recorded evidence are untouched by any of the fixes.
    let shipped = s.ticket(&tid("F200")).expect("ticket present");
    assert_eq!(shipped.status(), Status::Documented);
    assert!(
        s.ticket_evidence
            .get("F200")
            .is_some_and(|evs| !evs.is_empty()),
        "AC#5: the shipped feature's recorded evidence survived the burn-down"
    );
}
