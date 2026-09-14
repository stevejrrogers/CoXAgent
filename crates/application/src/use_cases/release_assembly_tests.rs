//! Unit tests for CXA-F231 — Atomic release-candidate assembly from verified
//! work, over the real domain/state types only.
//!
//! The Verified-complete predicate is the SA ruling on this ticket: the
//! type-aware terminal set (`Done | Documented | Verified`, exactly what
//! `selection::deps_satisfied` accepts). The transition table makes each
//! status unreachable for the wrong ticket type, so fixtures walk the only
//! legal routes — the same walks RunDev/RunTest drive in production. DoD
//! evidence is deliberately NOT part of the predicate: `ticket_evidence`
//! stores `waived` records when collection fails, so non-empty ≠ verified
//! (SA ruling); evidence stays a DoD artifact.
//!
//! Pure functions over state — no server, no store, no git, no network. This
//! file spinning up no harness is itself the proof that assembly is computed
//! purely from persisted state.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::state::ProjectState;
use crate::use_cases::release_assembly::{
    assemble, extract_ticket_refs, filter_verified_subjects, is_verified_complete, rc_members,
    RcAssembly, RcBundle,
};
use coxagent_domain::{
    Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};
use std::collections::BTreeSet;

const AT: &str = "2026-08-28T00:00:00Z";

// -------------------------------------------------------------------------
// Fixtures — every ticket built along its only legal transition route.
// -------------------------------------------------------------------------

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid id")
}

fn bug(id: &str) -> Ticket {
    Ticket::new(
        tid(id),
        TicketType::Bug,
        format!("defect {id}"),
        "repro: run the flow, observe the failure",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("valid ticket")
}

/// A bug driven to `Verified` — the state RunTestUseCase leaves a verified
/// bug in.
fn verified_bug(id: &str) -> Ticket {
    let mut t = bug(id);
    t.claim(Role::DevBug, "dev@host", AT)
        .expect("unclaimed bug is claimable");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("dev may mark a claimed bug fixed");
    t.transition_to(Role::Test, Status::Verified)
        .expect("a bug with no acceptance criteria verifies freely");
    t
}

/// A bug still at `Fixed`: regression passed, but it never reached the bug
/// lifecycle's terminal state, so it is NOT Verified-complete.
fn fixed_bug(id: &str) -> Ticket {
    let mut t = bug(id);
    t.claim(Role::DevBug, "dev@host", AT)
        .expect("unclaimed bug is claimable");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("dev may mark a claimed bug fixed");
    t
}

/// A bug claimed and in progress — an unverified prerequisite mid-flight.
fn in_progress_bug(id: &str) -> Ticket {
    let mut t = bug(id);
    t.claim(Role::DevBug, "dev@host", AT)
        .expect("unclaimed bug is claimable");
    t
}

fn feature(id: &str) -> Ticket {
    Ticket::new(
        tid(id),
        TicketType::Feature,
        format!("feature {id}"),
        "a slice of the feature the bundle ships",
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("valid ticket")
}

fn ready_gate(t: &mut Ticket) {
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            approach: "do it".to_owned(),
            ..TechnicalDesign::default()
        },
    )
    .expect("sa authors the design");
    t.transition_to(Role::Sa, Status::Ready)
        .expect("a technical design opens the ready gate");
}

/// A feature at `Done` — the feature lifecycle's completion status. It
/// carries NO DoD evidence record: evidence is a DoD artifact, not the
/// predicate (SA ruling on CXA-F231).
fn done_feature(id: &str) -> Ticket {
    let mut t = feature(id);
    ready_gate(&mut t);
    t.claim(Role::DevFeature, "dev@host", AT)
        .expect("ready feature is claimable");
    t.transition_to(Role::DevFeature, Status::Done)
        .expect("dev completes the feature");
    t
}

/// A feature fully through its lifecycle: Done then Docs-Documented.
fn documented_feature(id: &str) -> Ticket {
    let mut t = done_feature(id);
    t.transition_to(Role::Docs, Status::Documented)
        .expect("docs closes the lifecycle");
    t
}

/// A chore at `Done` — same lifecycle as a feature: the transition table
/// gives features and chores one route and one terminal status pair.
fn done_chore(id: &str) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Chore,
        format!("chore {id}"),
        "a chore the bundle ships",
        Priority::Low,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    ready_gate(&mut t);
    t.claim(Role::DevFeature, "dev@host", AT)
        .expect("ready chore is claimable");
    t.transition_to(Role::DevFeature, Status::Done)
        .expect("dev completes the chore");
    t
}

/// A feature still waiting on approval — not Verified-complete.
fn ready_feature(id: &str) -> Ticket {
    let mut t = feature(id);
    ready_gate(&mut t);
    t
}

/// Declare `dependent depends_on prerequisite` — the formal prerequisite
/// link SA owns (`field_permitted: "depends_on" => Role::Sa`).
fn depends_on(dependent: &mut Ticket, prerequisite: &str) {
    dependent
        .add_dependency(Role::Sa, tid(prerequisite))
        .expect("sa owns the dependency graph");
}

/// Declare a goal on the FINAL state and associate its ticket at `idx` with
/// it — the PO's goal gate (`field_permitted: "goal_id" => Role::Po`).
fn tag_goal(state: &mut ProjectState, idx: usize, title: &str) {
    let gid = state.add_goal(title).expect("goal declared");
    state.tickets[idx]
        .set_goal_id(Role::Po, gid)
        .expect("po owns goal associations");
}

fn state_with(tickets: Vec<Ticket>) -> ProjectState {
    ProjectState {
        tickets,
        ..ProjectState::default()
    }
}

/// Member lists compared as id strings, order-insensitively — the acceptance
/// criteria say nothing about ordering, so equality checks must not assume it
/// (the dependency-order test asserts ordering explicitly, separately).
fn sorted(members: &[TicketId]) -> Vec<String> {
    let mut v: Vec<String> = members.iter().map(|id| id.as_str().to_owned()).collect();
    v.sort();
    v
}

fn bundle_named<'a>(a: &'a RcAssembly, prefix: &str) -> &'a RcBundle {
    a.bundles
        .iter()
        .find(|b| b.name.starts_with(prefix))
        .unwrap_or_else(|| panic!("no bundle named {prefix} in {:?}", a.bundles))
}

// -------------------------------------------------------------------------
// The predicate — the SA-corrected, type-aware Verified-complete set
// -------------------------------------------------------------------------

#[test]
fn verified_complete_is_the_type_aware_terminal_set() {
    assert!(is_verified_complete(&verified_bug("CXC-B001")));
    assert!(!is_verified_complete(&fixed_bug("CXC-B002")));
    assert!(!is_verified_complete(&in_progress_bug("CXC-B003")));
    assert!(is_verified_complete(&done_feature("CXC-F010")));
    assert!(is_verified_complete(&done_chore("CXC-C001")));
    assert!(is_verified_complete(&documented_feature("CXC-F011")));
    assert!(!is_verified_complete(&ready_feature("CXC-F012")));
    // Rejected never qualifies: rejected work is dead, not pending. The only
    // legal route to Rejected for a feature is from Pending/Ready (PO call).
    let mut rejected = ready_feature("CXC-F013");
    rejected
        .transition_to(Role::Po, Status::Rejected)
        .expect("po may reject unstarted work");
    assert!(!is_verified_complete(&rejected));
}

// -------------------------------------------------------------------------
// AC1 — only Verified-complete tickets, grouped into named bundles, pure
// -------------------------------------------------------------------------

#[test]
fn ac1_lists_only_verified_complete_tickets_grouped_into_named_bundles() {
    let mut s = state_with(vec![
        verified_bug("CXC-B001"),
        done_feature("CXC-F010"),
        // Not Verified-complete: one gate short, mid-flight, unapproved.
        fixed_bug("CXC-B002"),
        in_progress_bug("CXC-B003"),
        ready_feature("CXC-F011"),
        done_feature("CXC-F012"),
    ]);
    tag_goal(&mut s, 5, "A new software product");

    // The flat list carries ONLY Verified-complete tickets…
    assert_eq!(
        sorted(&rc_members(&s)),
        ["CXC-B001", "CXC-F010", "CXC-F012"],
        "only Verified-complete tickets are candidates"
    );

    // …grouped into NAMED bundles: the goal line by name, goal-less work in
    // the explicit Unattributed bundle (F228: never silently dropped).
    let a = assemble(&s);
    assert_eq!(
        sorted(&bundle_named(&a, "G001").members),
        ["CXC-F012"],
        "the goal line names its own bundle"
    );
    assert_eq!(
        sorted(&bundle_named(&a, "Unattributed").members),
        ["CXC-B001", "CXC-F010"],
        "goal-less verified work lands in the explicit Unattributed bundle"
    );
}

// -------------------------------------------------------------------------
// AC2 — one ticket in/out re-evaluates bundle consistency
// -------------------------------------------------------------------------

#[test]
fn ac2_removing_the_support_dissolves_the_bundle_no_partial_feature_survives() {
    let feature_with_support = || {
        let mut f = done_feature("CXC-F010");
        depends_on(&mut f, "CXC-B001");
        f
    };
    // The support bug stays goal-less; the goal's ONLY member is the feature,
    // whose prerequisite resolves to the Verified-complete bug.
    let mut supported = state_with(vec![feature_with_support(), verified_bug("CXC-B001")]);
    tag_goal(&mut supported, 0, "A new software product");
    assert_eq!(
        sorted(&rc_members(&supported)),
        ["CXC-B001", "CXC-F010"],
        "a fully supported feature ships together with its support"
    );

    // Removing the ONE supporting ticket from scope re-evaluates consistency:
    // the feature's support is gone, so no partially-supported feature
    // survives assembly — and the block surfaces with its reason.
    let unsupported = state_with(vec![feature_with_support()]);
    assert_eq!(
        sorted(&rc_members(&unsupported)),
        Vec::<String>::new(),
        "no partially-supported feature survives"
    );
    let a = assemble(&unsupported);
    assert!(
        a.bundles.is_empty(),
        "no candidate is cut from partial scope"
    );
    assert_eq!(a.blocked.len(), 1);
    assert!(
        a.blocked[0].reason.contains("not in the project state"),
        "removal surfaces as an explicit block, not a silent shrink: {}",
        a.blocked[0].reason
    );

    // Adding the one ticket back re-evaluates the bundle to fully supported:
    // consistency is recomputed from state on every pass, never remembered.
    let restored = state_with(vec![feature_with_support(), verified_bug("CXC-B001")]);
    assert_eq!(
        sorted(&rc_members(&restored)),
        ["CXC-B001", "CXC-F010"],
        "adding the one ticket back restores the fully supported bundle"
    );
}

#[test]
fn ac2_a_shared_unverified_prerequisite_blocks_every_dependent_alike() {
    // Two goal bundles, each with one Verified-complete dependent on the SAME
    // goal-less in-progress prerequisite. It resolves for ALL of them or
    // none: letting it ride one bundle and block the other would leave it
    // unresolved across multiple bundles.
    let mut d1 = verified_bug("CXC-B002");
    let mut d2 = verified_bug("CXC-B003");
    depends_on(&mut d1, "CXC-B001");
    depends_on(&mut d2, "CXC-B001");

    // Control: the same two goal bundles WITHOUT the shared edges — both
    // dependents ship in their own goals, so any block below is caused by
    // the shared prerequisite, not by the goals.
    let mut control = state_with(vec![verified_bug("CXC-B002"), verified_bug("CXC-B003")]);
    tag_goal(&mut control, 0, "bundle one");
    tag_goal(&mut control, 1, "bundle two");
    assert_eq!(sorted(&rc_members(&control)), ["CXC-B002", "CXC-B003"]);

    // With the shared unverified prerequisite: both goals hold, both
    // dependents surface as blocked-with-reason naming the SAME prerequisite.
    let mut s = state_with(vec![in_progress_bug("CXC-B001"), d1, d2]);
    tag_goal(&mut s, 1, "bundle one");
    tag_goal(&mut s, 2, "bundle two");

    assert_eq!(
        sorted(&rc_members(&s)),
        Vec::<String>::new(),
        "a shared unverified prerequisite blocks every dependent alike"
    );
    let a = assemble(&s);
    assert!(
        a.bundles.is_empty(),
        "no bundle forms while shared scope is unresolved"
    );
    assert_eq!(a.blocked.len(), 2, "both dependents surface as blocked");
    for b in &a.blocked {
        assert!(
            b.reason.contains("CXC-B001"),
            "the reason names the shared prerequisite: {}",
            b.reason
        );
    }
}

// -------------------------------------------------------------------------
// AC3 — an unverified prerequisite blocks dependents, with a surfaced reason
// -------------------------------------------------------------------------

#[test]
fn ac3_unverified_prerequisite_blocks_dependent_with_explicit_reason() {
    // Control: the same verified dependent WITHOUT the edge is a candidate —
    // so any block below is caused by the prerequisite, not the ticket.
    let control = state_with(vec![fixed_bug("CXC-B001"), verified_bug("CXC-B002")]);
    assert_eq!(sorted(&rc_members(&control)), ["CXC-B002"]);

    let mut dependent = verified_bug("CXC-B002");
    depends_on(&mut dependent, "CXC-B001");
    let s = state_with(vec![fixed_bug("CXC-B001"), dependent]);

    assert_eq!(
        sorted(&rc_members(&s)),
        Vec::<String>::new(),
        "the unverified prerequisite blocks every dependent candidate"
    );
    let a = assemble(&s);
    assert!(a.bundles.is_empty(), "no broken partial scope is assembled");
    assert_eq!(a.blocked.len(), 1);
    assert_eq!(a.blocked[0].ticket, tid("CXC-B002"));
    assert!(
        a.blocked[0].reason.contains("CXC-B001") && a.blocked[0].reason.contains("Fixed"),
        "the reason names the prerequisite and its status: {}",
        a.blocked[0].reason
    );

    // A prerequisite missing from state entirely blocks too — state is
    // external input, never trusted.
    let mut orphan = verified_bug("CXC-B004");
    depends_on(&mut orphan, "CXC-B999");
    let gone = state_with(vec![orphan]);
    let a = assemble(&gone);
    assert_eq!(sorted(&rc_members(&gone)), Vec::<String>::new());
    assert!(
        a.blocked[0].reason.contains("CXC-B999")
            && a.blocked[0].reason.contains("not in the project state"),
        "a vanished prerequisite surfaces with its own reason: {}",
        a.blocked[0].reason
    );
}

// -------------------------------------------------------------------------
// AC4 — removing one ticket touches only candidates actually depending on it
// -------------------------------------------------------------------------

#[test]
fn ac4_removing_one_ticket_leaves_unrelated_bundles_unchanged() {
    let mut s = state_with(vec![
        verified_bug("CXC-B001"),
        verified_bug("CXC-B002"),
        verified_bug("CXC-B003"), // goal-less → Unattributed
    ]);
    tag_goal(&mut s, 0, "bundle one");
    tag_goal(&mut s, 1, "bundle two");

    let before = assemble(&s);
    assert_eq!(before.bundles.len(), 3, "three independent candidates");

    // Remove the ONE ticket of bundle two: only its own candidacy is
    // affected — the other bundles are unchanged, down to member order.
    let mut after_state = state_with(vec![verified_bug("CXC-B001"), verified_bug("CXC-B003")]);
    tag_goal(&mut after_state, 0, "bundle one");
    let after = assemble(&after_state);

    assert_eq!(after.bundles.len(), 2);
    assert_eq!(
        bundle_named(&after, "G001"),
        bundle_named(&before, "G001"),
        "the unrelated goal bundle is unchanged"
    );
    assert_eq!(
        bundle_named(&after, "Unattributed"),
        bundle_named(&before, "Unattributed"),
        "the unrelated unattributed bundle is unchanged"
    );
}

// -------------------------------------------------------------------------
// AC5 — zero Verified-complete tickets: explicit empty state, no candidate
// -------------------------------------------------------------------------

#[test]
fn ac5_zero_verified_complete_tickets_produce_an_explicit_empty_state() {
    // Work exists, but nothing reached a terminal status: the surface says so
    // explicitly — empty bundles AND empty blocked, never a fabricated bundle.
    let s = state_with(vec![fixed_bug("CXC-B001"), in_progress_bug("CXC-B002")]);
    let a = assemble(&s);
    assert!(a.is_empty(), "the empty state is explicit");
    assert!(rc_members(&s).is_empty());

    // The fully empty backlog is the same explicit empty state.
    let empty = state_with(Vec::new());
    assert!(assemble(&empty).is_empty());
}

#[test]
fn a_goal_with_no_surviving_work_fabricates_no_bundle() {
    // Goals are declared before any work exists — and a goal whose every
    // member was Rejected has no surviving work either. Neither may emit an
    // EMPTY bundle: that would fake a candidate out of a declared goal line
    // and break AC5's explicit empty state.
    let mut untouched = state_with(Vec::new());
    untouched.add_goal("declared, no work yet").expect("goal");
    assert!(
        assemble(&untouched).bundles.is_empty(),
        "an untouched goal line proposes no bundle"
    );

    let mut s = state_with(Vec::new());
    let gid = s.add_goal("everything rejected").expect("goal");
    let mut first = ready_feature("CXC-F010");
    first.set_goal_id(Role::Po, gid.clone()).expect("po");
    first
        .transition_to(Role::Po, Status::Rejected)
        .expect("po rejects from ready");
    let mut second = ready_feature("CXC-F011");
    second.set_goal_id(Role::Po, gid).expect("po");
    second
        .transition_to(Role::Po, Status::Rejected)
        .expect("po rejects from ready");
    s.tickets = vec![first, second];

    let a = assemble(&s);
    assert!(
        a.bundles.is_empty(),
        "a goal with only Rejected members proposes no bundle"
    );
    assert!(a.is_empty(), "the empty state stays explicit");
}

#[test]
fn rejected_members_neither_block_nor_ride_their_goal_bundle() {
    // The atomicity contract scopes to NON-Rejected members: rejected work
    // is dead, so it cannot hold its goal hostage — and it never ships.
    let mut s = state_with(Vec::new());
    let gid = s.add_goal("one live, one dead").expect("goal");
    let mut live = verified_bug("CXC-B001");
    live.set_goal_id(Role::Po, gid.clone()).expect("po");
    let mut dead = ready_feature("CXC-F010");
    dead.set_goal_id(Role::Po, gid).expect("po");
    dead.transition_to(Role::Po, Status::Rejected)
        .expect("po rejects from ready");
    s.tickets = vec![live, dead];

    let a = assemble(&s);
    assert_eq!(a.bundles.len(), 1);
    assert_eq!(
        bundle_named(&a, "G001").members,
        vec![tid("CXC-B001")],
        "the goal ships its live verified member alone"
    );
}

// -------------------------------------------------------------------------
// Dependency ordering — prerequisites surface before their dependents
// -------------------------------------------------------------------------

#[test]
fn bundle_members_are_ordered_prerequisites_first() {
    let mut f = done_feature("CXC-F010");
    depends_on(&mut f, "CXC-B001");
    let mut g = done_feature("CXC-F011");
    depends_on(&mut g, "CXC-F010");
    let s = state_with(vec![f, g, verified_bug("CXC-B001")]);

    let a = assemble(&s);
    let unattributed = bundle_named(&a, "Unattributed");
    assert_eq!(
        unattributed
            .members
            .iter()
            .map(TicketId::as_str)
            .collect::<Vec<_>>(),
        ["CXC-B001", "CXC-F010", "CXC-F011"],
        "cross-ticket dependency order is surfaced upfront: support, then its \
         dependent, then the dependent's dependent"
    );
}

// -------------------------------------------------------------------------
// Commit-subject correlation — the cut's manifest gate (honest-by-default)
// -------------------------------------------------------------------------

#[test]
fn extract_ticket_refs_parses_realistic_conventional_subjects() {
    assert_eq!(
        extract_ticket_refs("feat(cxa): wire REST store #CXA-F228"),
        Some(vec!["CXA-F228".to_owned()])
    );
    assert_eq!(
        extract_ticket_refs("fix(CXA-B084): CXA-B083 not actually fixed: squatting :8101 still"),
        Some(vec!["CXA-B084".to_owned(), "CXA-B083".to_owned()])
    );
    assert_eq!(
        extract_ticket_refs("fix(cxa-b001): lowercase refs normalise"),
        Some(vec!["CXA-B001".to_owned()])
    );
    // Non-ref subjects parse to None — merges, release markers, plain chores.
    assert_eq!(extract_ticket_refs("Merge branch 'x'"), None);
    assert_eq!(extract_ticket_refs("release: v2.27.0"), None);
    assert_eq!(extract_ticket_refs("chore: tidy"), None);
}

#[test]
fn filter_included_keeps_exactly_refbearing_verified_subjects() {
    let subjects: Vec<String> = [
        "feat(cxa): wire REST store #CXA-F228", // verified ref → in
        "fix(cxa): crash on save #CXA-B001",    // verified ref → in
        "fix(cxa): partial support #CXA-B002",  // unverified ref → out
        "fix(CXA-B084): CXA-B083 not actually fixed", // one unverified taints
        "chore: no ticket ref at all",          // ref-less → out
        "Merge branch 'main'",                  // ref-less → out
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let verified: BTreeSet<String> = ["CXA-F228", "CXA-B001", "CXA-B083"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

    let m = filter_verified_subjects(&subjects, &verified);
    assert_eq!(m.included, [subjects[0].clone(), subjects[1].clone()]);
    assert_eq!(m.included_ids, ["CXA-F228", "CXA-B001"]);
    assert_eq!(
        m.excluded,
        [
            subjects[2].clone(),
            subjects[3].clone(),
            subjects[4].clone(),
            subjects[5].clone()
        ]
    );
    assert_eq!(
        m.excluded_ids,
        ["CXA-B002", "CXA-B084"],
        "only the unverified ids that caused exclusions, deduped"
    );
}
