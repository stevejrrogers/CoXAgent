//! TDD tests for CXA-F231: Atomic release-candidate assembly from verified work.
//!
//! These tests encode the ticket's acceptance criteria EXACTLY against the
//! shipped implementation (`release_candidates.rs`) — the single source of
//! truth they import and exercise. They are PURE functions over the persisted
//! state types: every fixture is built through the domain aggregate API the
//! codebase already has — no server, no host harness, no network port.
//!
//! AC → test map:
//! - AC1 (lists only Verified-complete tickets, grouped into named bundles,
//!   computed purely from state): [`assembly_lists_only_verified_tickets_grouped_into_named_bundles`]
//! - AC2 (adding/removing one ticket re-evaluates bundle consistency — no
//!   partially-supported feature survives; no shared unverified prerequisite
//!   left unresolved across multiple bundles):
//!   [`removing_one_ticket_drops_the_partially_supported_feature_until_it_is_readded`],
//!   [`a_shared_unverified_prerequisite_is_surfaced_on_every_bundle_that_needs_it`]
//! - AC3 (an unverified prerequisite blocks every dependent candidate with an
//!   explicit surfaced reason): [`an_unverified_prerequisite_blocks_the_dependent_candidate_with_an_explicit_reason`]
//! - AC4 (removing any single ticket affects only candidates actually
//!   depending on it): [`removing_a_ticket_leaves_unrelated_bundles_unchanged`],
//!   [`removing_a_ticket_affects_only_candidates_that_actually_depend_on_it`]
//! - AC5 (zero Verified-complete tickets → explicit empty state, no
//!   candidate): [`with_zero_verified_tickets_the_result_produces_no_candidate`]
//! - SA-corrected Verified-complete set (features/chores end at
//!   `Done`/`Documented`, not bug-only `Verified`; Rejected ships nowhere,
//!   OnHold keeps its goal line parked):
//!   [`a_done_feature_qualifies_as_verified_complete_for_its_goal_bundle`],
//!   [`a_rejected_ticket_neither_joins_nor_blocks_its_goal_bundle`],
//!   [`an_on_hold_ticket_keeps_its_goal_line_from_proposing`]

use crate::release_candidates::{is_verified_complete, release_candidates, ReleaseCandidate};
use crate::state::ProjectState;
use coxagent_domain::{
    Complexity, GoalId, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};

/// The `G001`-style id `ProjectState::add_goal` mints in declaration order.
fn goal(n: u8) -> GoalId {
    GoalId::new(format!("G{n:03}")).expect("gid")
}

/// A bug shepherded to `Verified` — the only status that reaches it — with its
/// verified-DoD evidence persisted: every acceptance criterion demonstrated by
/// a PASSING test case carrying evidence. Built entirely through the public
/// aggregate API, exactly like the real verification path (the aggregate's
/// `check_covered` gate refuses a `Verified` transition without it).
fn verified_bug(id: &str, goal_id: Option<GoalId>, depends_on: &[&str]) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("id"),
        TicketType::Bug,
        format!("fix {id}"),
        "fixture",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    if let Some(g) = goal_id {
        t.set_goal_id(Role::System, g).expect("bind goal");
    }
    for dep in depends_on {
        t.add_dependency(Role::System, TicketId::new(*dep).expect("dep id"))
            .expect("dependency");
    }
    t.set_acceptance_criteria(vec!["ac one".to_owned()]);
    t.ensure_test_cases_from_acceptance();
    assert!(t.set_test_case_result(
        "ac one",
        true,
        Some("verified with evidence".to_owned()),
        None,
        "t0".into(),
    ));
    for to in [Status::InProgress, Status::Fixed, Status::Verified] {
        t.transition_to(Role::System, to)
            .expect("system walks to verified");
    }
    t
}

/// A bug that has NOT reached `Verified`, parked at one of the persisted
/// mid-lifecycle statuses (state between cycles, buildable from real data).
fn unverified_bug(id: &str, at: Status, goal_id: Option<GoalId>) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("id"),
        TicketType::Bug,
        format!("fix {id}"),
        "fixture",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    if let Some(g) = goal_id {
        t.set_goal_id(Role::System, g).expect("bind goal");
    }
    match at {
        Status::Open => {}
        Status::InProgress => {
            t.transition_to(Role::System, Status::InProgress)
                .expect("walk");
        }
        Status::Fixed => {
            t.transition_to(Role::System, Status::InProgress)
                .expect("walk");
            t.transition_to(Role::System, Status::Fixed).expect("walk");
        }
        _ => panic!("fixture supports Open/InProgress/Fixed only, got {at:?}"),
    }
    t
}

/// A feature still awaiting design (the feature lifecycle never reaches
/// `Verified` — a Pending feature is persisted, not Verified-complete).
fn pending_feature(id: &str, goal_id: Option<GoalId>) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("id"),
        TicketType::Feature,
        format!("feature {id}"),
        "fixture",
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("ticket");
    if let Some(g) = goal_id {
        t.set_goal_id(Role::System, g).expect("bind goal");
    }
    t
}

/// A feature walked to `Done` — the feature lifecycle's terminal-complete
/// status. The SA-corrected Verified-complete set: features/chores end at
/// `Done`/`Documented` (`transitions.rs` gives them no `Verified` edge), so a
/// Done feature is RC-eligible exactly like a Verified bug.
fn done_feature(id: &str, goal_id: Option<GoalId>) -> Ticket {
    let mut t = pending_feature(id, goal_id);
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            approach: "fixture design".to_owned(),
            ..TechnicalDesign::default()
        },
    )
    .expect("attach design");
    for to in [Status::Ready, Status::InProgress, Status::Done] {
        t.transition_to(Role::System, to)
            .expect("system walks to done");
    }
    t
}

fn state_with(goal_titles: &[&str], tickets: Vec<Ticket>) -> ProjectState {
    let mut s = ProjectState::default();
    for title in goal_titles {
        s.add_goal(title).expect("goal");
    }
    s.tickets = tickets;
    s
}

/// The one bundle whose members include `ticket`.
fn bundle_containing<'a>(board: &'a [ReleaseCandidate], ticket: &str) -> &'a ReleaseCandidate {
    board
        .iter()
        .find(|c| c.tickets.iter().any(|t| t.as_str() == ticket))
        .expect("bundle containing the ticket")
}

/// The failed-regression walk back out of `Verified` — the legal way a single
/// verified ticket leaves the verified set (PO reopens a bug).
fn reopen(ticket_id: &str, state: &mut ProjectState) {
    let id = TicketId::new(ticket_id).expect("id");
    state
        .ticket_mut(&id)
        .expect("ticket")
        .transition_to(Role::Po, Status::Open)
        .expect("reopen");
}

/// The legal re-verification walk back into `Verified` (evidence is already
/// persisted on the ticket, so `check_covered` passes again).
fn reverify(ticket_id: &str, state: &mut ProjectState) {
    let id = TicketId::new(ticket_id).expect("id");
    let t = state.ticket_mut(&id).expect("ticket");
    for to in [Status::InProgress, Status::Fixed, Status::Verified] {
        t.transition_to(Role::System, to).expect("re-verify");
    }
}

#[test]
fn assembly_lists_only_verified_tickets_grouped_into_named_bundles() {
    // AC1 — computed purely from state: an in-memory ProjectState is the whole
    // input; no server, no harness, no port is spun up anywhere in this test.
    let search = goal(1);
    let export = goal(2);
    let billing = goal(3);
    let s1 = verified_bug("RC-001", Some(search.clone()), &[]);
    let s2 = verified_bug("RC-002", Some(search.clone()), &[]);
    let e1 = verified_bug("RC-003", Some(export.clone()), &[]);
    // Not Verified-complete: a Pending feature and bugs mid-bug-lifecycle,
    // all declared against Billing — that goal line is only partially
    // supported, so none of its tickets may surface in any bundle.
    let feature = pending_feature("RC-004", Some(billing.clone()));
    let fixed = unverified_bug("RC-005", Status::Fixed, Some(billing.clone()));
    let open = unverified_bug("RC-006", Status::Open, Some(billing.clone()));
    let state = state_with(
        &["Search", "Export", "Billing"],
        vec![s1, s2, e1, feature, fixed, open],
    );

    let board = release_candidates(&state);

    assert_eq!(
        board.len(),
        2,
        "one bundle per fully-verified goal: {board:?}"
    );
    for cand in &board {
        assert!(!cand.name.trim().is_empty(), "bundles are named: {cand:?}");
        assert!(
            !cand.tickets.is_empty(),
            "a bundle groups its tickets: {cand:?}"
        );
        assert!(
            cand.blocked.is_none(),
            "nothing blocks a fully-verified independent bundle: {cand:?}"
        );
        for tid in &cand.tickets {
            let t = state
                .tickets
                .iter()
                .find(|t2| t2.id() == tid)
                .expect("member ticket exists in state");
            assert!(
                is_verified_complete(t),
                "only Verified-complete tickets are listed, {tid} is not"
            );
        }
    }
    let members: Vec<String> = board
        .iter()
        .flat_map(|c| c.tickets.iter().map(TicketId::to_string))
        .collect();
    for id in ["RC-004", "RC-005", "RC-006"] {
        assert!(
            !members.iter().any(|m| m == id),
            "{id} is not Verified-complete and must not be listed"
        );
    }
    // Grouping: the Search bundle carries exactly its own two tickets.
    let search_bundle = bundle_containing(&board, "RC-001");
    assert_eq!(search_bundle.tickets.len(), 2, "{search_bundle:?}");
    assert!(search_bundle.tickets.iter().any(|t| t.as_str() == "RC-002"));
}

#[test]
fn removing_one_ticket_drops_the_partially_supported_feature_until_it_is_readded() {
    // AC2 — removing (then re-adding) ONE ticket within a proposed bundle
    // re-evaluates bundle consistency from current state.
    let g = goal(1);
    let g1 = verified_bug("RC-101", Some(g.clone()), &[]);
    let g2 = verified_bug("RC-102", Some(g.clone()), &[]);
    let mut state = state_with(&["Bundle feature"], vec![g1, g2]);

    let before = release_candidates(&state);
    assert_eq!(before.len(), 1, "fully verified -> proposed: {before:?}");
    let proposed = before.first().expect("one bundle");
    assert!(proposed.blocked.is_none());
    let name = proposed.name.clone();
    let members = proposed.tickets.clone();

    // Remove one ticket from the proposed bundle (legal Verified -> Open).
    reopen("RC-102", &mut state);
    let after = release_candidates(&state);
    assert!(
        after.is_empty(),
        "a partially-supported feature must not survive as a bundle: {after:?}"
    );

    // Adding the ticket back re-evaluates to the same proposed bundle.
    reverify("RC-102", &mut state);
    let restored = release_candidates(&state);
    assert_eq!(restored.len(), 1, "{restored:?}");
    let restored = restored.first().expect("one bundle");
    assert_eq!(restored.name, name, "the bundle keeps its name");
    assert_eq!(restored.tickets, members, "the bundle keeps its members");
    assert!(restored.blocked.is_none());
}

#[test]
fn a_shared_unverified_prerequisite_is_surfaced_on_every_bundle_that_needs_it() {
    // AC2 — a shared unverified prerequisite must not appear unresolved across
    // multiple bundles: EVERY bundle depending on it carries the surfaced block.
    let p = unverified_bug("RC-201", Status::InProgress, None);
    let x1 = verified_bug("RC-202", Some(goal(1)), &["RC-201"]);
    let y1 = verified_bug("RC-203", Some(goal(2)), &["RC-201"]);
    let state = state_with(
        &["Consumes foundation X", "Consumes foundation Y"],
        vec![p, x1, y1],
    );

    let board = release_candidates(&state);

    assert_eq!(
        board.len(),
        2,
        "both bundles are internally complete: {board:?}"
    );
    for cand in &board {
        let blocked = cand
            .blocked
            .as_ref()
            .expect("the shared unverified prerequisite blocks this bundle too");
        assert_eq!(
            blocked.prerequisite.as_str(),
            "RC-201",
            "the block names the shared prerequisite"
        );
        assert!(
            !blocked.reason.trim().is_empty(),
            "the block carries an explicit reason: {cand:?}"
        );
        assert!(
            !cand.tickets.iter().any(|t| t.as_str() == "RC-201"),
            "no broken partial scope: the unverified prerequisite ships in no bundle"
        );
    }
}

#[test]
fn an_unverified_prerequisite_blocks_the_dependent_candidate_with_an_explicit_reason() {
    // AC3 — the dependent bundle is proposed (its own scope is fully
    // verified) but explicitly blocked, not shipped as broken partial scope.
    let p = unverified_bug("RC-301", Status::InProgress, None);
    let w1 = verified_bug("RC-302", Some(goal(1)), &["RC-301"]);
    let w2 = verified_bug("RC-303", Some(goal(1)), &[]);
    let state = state_with(&["Dependent feature"], vec![p, w1, w2]);

    let board = release_candidates(&state);

    assert_eq!(
        board.len(),
        1,
        "the dependent bundle is still proposed: {board:?}"
    );
    let cand = board.first().expect("one bundle");
    assert!(
        cand.tickets.iter().all(|t| t.as_str() != "RC-301"),
        "the unverified prerequisite is not part of the bundle"
    );
    let blocked = cand
        .blocked
        .as_ref()
        .expect("the unverified prerequisite blocks the candidate");
    assert_eq!(blocked.prerequisite.as_str(), "RC-301");
    assert!(
        !blocked.reason.trim().is_empty(),
        "the surfaced reason is explicit: {cand:?}"
    );
}

#[test]
fn removing_a_ticket_leaves_unrelated_bundles_unchanged() {
    // AC4 — removing ONE ticket affects only its own bundle.
    let a1 = verified_bug("RC-401", Some(goal(1)), &[]);
    let a2 = verified_bug("RC-402", Some(goal(1)), &[]);
    let b1 = verified_bug("RC-403", Some(goal(2)), &[]);
    let c1 = verified_bug("RC-404", Some(goal(3)), &[]);
    let mut state = state_with(&["Alpha", "Beta", "Gamma"], vec![a1, a2, b1, c1]);

    let before = release_candidates(&state);
    assert_eq!(before.len(), 3, "{before:?}");
    let beta = bundle_containing(&before, "RC-403");
    let gamma = bundle_containing(&before, "RC-404");

    reopen("RC-401", &mut state);
    let after = release_candidates(&state);

    assert!(
        !after.iter().any(|c| c
            .tickets
            .iter()
            .any(|t| t.as_str() == "RC-401" || t.as_str() == "RC-402")),
        "Alpha is now partially supported — its bundle does not survive: {after:?}"
    );
    assert_eq!(
        bundle_containing(&after, "RC-403"),
        beta,
        "Beta is unrelated to the removed ticket — unchanged"
    );
    assert_eq!(
        bundle_containing(&after, "RC-404"),
        gamma,
        "Gamma is unrelated to the removed ticket — unchanged"
    );
}

#[test]
fn removing_a_ticket_affects_only_candidates_that_actually_depend_on_it() {
    // AC4 — Beta depends on Alpha's ticket via depends_on; Alpha's removal
    // affects Alpha (partial feature) and Beta (now blocked), nothing else.
    let a1 = verified_bug("RC-501", Some(goal(1)), &[]);
    let b1 = verified_bug("RC-502", Some(goal(2)), &["RC-501"]);
    let c1 = verified_bug("RC-503", Some(goal(3)), &[]);
    let mut state = state_with(
        &["Foundation", "Built on it", "Independent"],
        vec![a1, b1, c1],
    );

    let before = release_candidates(&state);
    assert_eq!(before.len(), 3, "{before:?}");
    let gamma = bundle_containing(&before, "RC-503").clone();

    reopen("RC-501", &mut state);
    let after = release_candidates(&state);

    assert!(
        !after
            .iter()
            .any(|c| c.tickets.iter().any(|t| t.as_str() == "RC-501")),
        "the foundation's own bundle does not survive its removal: {after:?}"
    );
    let beta = bundle_containing(&after, "RC-502");
    let blocked = beta
        .blocked
        .as_ref()
        .expect("the candidate that depended on the removed ticket is blocked");
    assert_eq!(blocked.prerequisite.as_str(), "RC-501");
    assert_eq!(
        bundle_containing(&after, "RC-503"),
        &gamma,
        "the candidate depending on nothing is untouched"
    );
}

#[test]
fn with_zero_verified_tickets_the_result_produces_no_candidate() {
    // AC5 — zero Verified-complete tickets: an explicit empty state at the
    // pure layer (the surface renders it as the empty state), no candidate.
    let empty = ProjectState::default();
    assert!(
        release_candidates(&empty).is_empty(),
        "no candidate from a project with no tickets"
    );

    let open = unverified_bug("RC-601", Status::Open, Some(goal(1)));
    let fixed = unverified_bug("RC-602", Status::Fixed, Some(goal(1)));
    let state = state_with(&["Nothing verified yet"], vec![open, fixed]);
    let board = release_candidates(&state);
    assert!(
        board.is_empty(),
        "zero Verified-complete tickets -> no candidate: {board:?}"
    );
}

#[test]
fn a_done_feature_qualifies_as_verified_complete_for_its_goal_bundle() {
    // The SA-corrected AC1 set: features/chores are Verified-complete at
    // `Done`/`Documented` — a goal mixing a Done feature with a Verified bug
    // proposes ONE bundle carrying both, and none of them is a bug at
    // `Verified`.
    let g = goal(1);
    let f = done_feature("RC-701", Some(g.clone()));
    let b = verified_bug("RC-702", Some(g), &[]);
    let state = state_with(&["Mixed terminal states"], vec![f, b]);

    let board = release_candidates(&state);

    assert_eq!(board.len(), 1, "{board:?}");
    let cand = board.first().expect("one bundle");
    assert_eq!(cand.tickets.len(), 2, "{cand:?}");
    assert!(cand.blocked.is_none(), "{cand:?}");
}

#[test]
fn a_rejected_ticket_neither_joins_nor_blocks_its_goal_bundle() {
    // Rejected is scope a PO removed — it neither ships nor keeps its goal
    // line permanently incomplete, so the verified remainder still bundles.
    let g = goal(1);
    let keep = verified_bug("RC-711", Some(g.clone()), &[]);
    let mut rejected = pending_feature("RC-712", Some(g));
    rejected
        .transition_to(Role::Po, Status::Rejected)
        .expect("reject");
    let state = state_with(&["Partly rejected"], vec![keep, rejected]);

    let board = release_candidates(&state);

    assert_eq!(
        board.len(),
        1,
        "only the verified remainder bundles: {board:?}"
    );
    let cand = board.first().expect("one bundle");
    assert_eq!(cand.tickets.len(), 1, "{cand:?}");
    assert_eq!(cand.tickets.first().expect("member").as_str(), "RC-711");
    assert!(cand.blocked.is_none(), "{cand:?}");
}

#[test]
fn an_on_hold_ticket_keeps_its_goal_line_from_proposing() {
    // OnHold is parked-but-planned work (it resumes, unlike Rejected): the
    // goal line stays incomplete and proposes nothing until it resolves.
    let g = goal(1);
    let done = done_feature("RC-721", Some(g.clone()));
    let mut parked = pending_feature("RC-722", Some(g));
    parked
        .transition_to(Role::Po, Status::OnHold)
        .expect("hold");
    let state = state_with(&["Parked line"], vec![done, parked]);

    let board = release_candidates(&state);

    assert!(
        board.is_empty(),
        "a parked member keeps the line out: {board:?}"
    );
}
