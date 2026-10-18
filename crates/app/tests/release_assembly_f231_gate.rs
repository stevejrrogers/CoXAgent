//! CXA-F231 acceptance gate — "Atomic release-candidate assembly from
//! verified work".
//!
//! Written against the SHIPPED module as its single source of truth
//! (`coxagent_application::use_cases::release_assembly`, per the team's
//! standing decision that every acceptance gate imports and exercises the
//! shipped module — never a private copy). Pure over the state/domain types:
//! no server, no store, no git, no network port.
//!
//! Verified-complete is the SA ruling on this ticket: the type-aware terminal
//! set (`Done | Documented | Verified`, what `selection::deps_satisfied`
//! already accepts). A bug only ever reaches `Verified` and a feature/chore
//! only `Done | Documented`, because the transition table has no other edges
//! — so fixtures walk the legal routes and the predicate arms both
//! lifecycles with one match.
//!
//! AC#1 lists only Verified-complete tickets grouped into named bundles (a
//! goal line per bundle, goal-less work in the explicit `Unattributed`
//! bundle), computed purely from state. AC#2 re-evaluates bundle consistency
//! when one ticket enters or leaves a proposed bundle. AC#3 surfaces an
//! unverified prerequisite as a blocked candidate with a reason, never as
//! broken partial scope. AC#4 scopes every removal to the candidates that
//! actually depend on the removed ticket. AC#5 renders an explicit empty
//! state — no candidate — from zero Verified-complete tickets.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::config::ReleasesConfig;
use coxagent_application::selection;
use coxagent_application::state::ProjectState;
use coxagent_application::use_cases::release_assembly::{
    assemble, extract_ticket_refs, filter_verified_subjects, is_verified_complete, rc_members,
    RcAssembly, RcBundle,
};
use coxagent_domain::{
    Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};
use std::collections::BTreeSet;

const AT: &str = "2026-08-28T00:00:00Z";

// ---------------------------------------------------------------------------
// Fixtures — the only legal transition routes (RunDev/RunTest walk these).
// ---------------------------------------------------------------------------

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

fn verified_bug(id: &str) -> Ticket {
    let mut t = bug(id);
    t.claim(Role::DevBug, "dev@host", AT).expect("claim");
    t.transition_to(Role::DevBug, Status::Fixed).expect("fix");
    t.transition_to(Role::Test, Status::Verified)
        .expect("verify");
    t
}

fn fixed_bug(id: &str) -> Ticket {
    let mut t = bug(id);
    t.claim(Role::DevBug, "dev@host", AT).expect("claim");
    t.transition_to(Role::DevBug, Status::Fixed).expect("fix");
    t
}

fn done_feature(id: &str) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Feature,
        format!("feature {id}"),
        "a slice of the feature the bundle ships",
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("valid ticket");
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            approach: "do it".to_owned(),
            ..TechnicalDesign::default()
        },
    )
    .expect("design");
    t.transition_to(Role::Sa, Status::Ready).expect("ready");
    t.claim(Role::DevFeature, "dev@host", AT).expect("claim");
    t.transition_to(Role::DevFeature, Status::Done)
        .expect("done");
    t
}

fn ready_feature(id: &str) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Feature,
        format!("feature {id}"),
        "a slice of the feature the bundle ships",
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("valid ticket");
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            approach: "do it".to_owned(),
            ..TechnicalDesign::default()
        },
    )
    .expect("design");
    t.transition_to(Role::Sa, Status::Ready).expect("ready");
    t
}

fn documented_feature(id: &str) -> Ticket {
    let mut t = done_feature(id);
    t.transition_to(Role::Docs, Status::Documented)
        .expect("docs closes the lifecycle");
    t
}

fn depends_on(dependent: &mut Ticket, prerequisite: &str) {
    dependent
        .add_dependency(Role::Sa, tid(prerequisite))
        .expect("sa owns the dependency graph");
}

fn tag_goal(state: &mut ProjectState, idx: usize, title: &str) {
    let gid = state.add_goal(title).expect("goal declared");
    state.tickets[idx]
        .set_goal_id(Role::Po, gid)
        .expect("po owns goal associations");
}

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

// ---------------------------------------------------------------------------
// AC#1 — only Verified-complete tickets, grouped into named bundles, pure
// ---------------------------------------------------------------------------

#[test]
fn ac1_lists_only_verified_complete_tickets_grouped_into_named_bundles() {
    let mut s = ProjectState {
        tickets: vec![
            verified_bug("CXC-B001"),
            done_feature("CXC-F010"),
            fixed_bug("CXC-B002"), // one gate short of its terminal status
        ],
        ..ProjectState::default()
    };
    tag_goal(&mut s, 1, "A new software product");

    assert_eq!(
        sorted(&rc_members(&s)),
        ["CXC-B001", "CXC-F010"],
        "assembly lists ONLY Verified-complete tickets"
    );
    let a = assemble(&s);
    assert_eq!(
        sorted(&bundle_named(&a, "G001").members),
        ["CXC-F010"],
        "the goal line names its bundle"
    );
    assert_eq!(
        sorted(&bundle_named(&a, "Unattributed").members),
        ["CXC-B001"],
        "goal-less verified work is explicit, never dropped"
    );
    // Purity: the same state always assembles to the same surface.
    assert_eq!(a, assemble(&s));
}

// ---------------------------------------------------------------------------
// AC#2 — one ticket in/out re-evaluates bundle consistency
// ---------------------------------------------------------------------------

#[test]
fn ac2_removing_one_ticket_re_evaluates_the_bundle_no_partial_scope_survives() {
    let feature_with_support = || {
        let mut f = done_feature("CXC-F010");
        depends_on(&mut f, "CXC-B001");
        f
    };
    let mut s = ProjectState {
        tickets: vec![feature_with_support(), verified_bug("CXC-B001")],
        ..ProjectState::default()
    };
    tag_goal(&mut s, 0, "A new software product");
    assert_eq!(
        sorted(&rc_members(&s)),
        ["CXC-B001", "CXC-F010"],
        "the feature ships together with its supporting fix"
    );

    // Removing the ONE supporting ticket re-evaluates: no partially-supported
    // feature survives, and no candidate is produced from the partial scope.
    let s = ProjectState {
        tickets: vec![feature_with_support()],
        ..ProjectState::default()
    };
    assert!(rc_members(&s).is_empty());
    let a = assemble(&s);
    assert!(a.bundles.is_empty());
    assert_eq!(a.blocked.len(), 1, "the removal surfaces as a reason");
}

#[test]
fn ac2_a_shared_unverified_prerequisite_blocks_both_bundles_until_it_ships_once() {
    let mut d1 = verified_bug("CXC-B002");
    let mut d2 = verified_bug("CXC-B003");
    depends_on(&mut d1, "CXC-B001");
    depends_on(&mut d2, "CXC-B001");
    let mut s = ProjectState {
        tickets: vec![fixed_bug("CXC-B001"), d1.clone(), d2.clone()],
        ..ProjectState::default()
    };
    tag_goal(&mut s, 1, "bundle one");
    tag_goal(&mut s, 2, "bundle two");

    assert!(
        rc_members(&s).is_empty(),
        "the shared unverified prerequisite resolves for ALL dependents or none"
    );
    let a = assemble(&s);
    assert_eq!(a.blocked.len(), 2);
    assert!(
        a.blocked.iter().all(|b| b.reason.contains("CXC-B001")),
        "every blocked reason names the shared prerequisite"
    );

    // The prerequisite verifies once → BOTH bundles release together.
    let s = ProjectState {
        tickets: vec![verified_bug("CXC-B001"), d1, d2],
        ..ProjectState::default()
    };
    assert_eq!(
        sorted(&rc_members(&s)),
        ["CXC-B001", "CXC-B002", "CXC-B003"]
    );
}

// ---------------------------------------------------------------------------
// AC#3 — unverified prerequisite blocks dependents with a surfaced reason
// ---------------------------------------------------------------------------

#[test]
fn ac3_unverified_prerequisite_blocks_dependent_with_explicit_reason() {
    // Control: without the edge the same dependent is a candidate.
    let control = ProjectState {
        tickets: vec![fixed_bug("CXC-B001"), verified_bug("CXC-B002")],
        ..ProjectState::default()
    };
    assert_eq!(sorted(&rc_members(&control)), ["CXC-B002"]);

    let mut dependent = verified_bug("CXC-B002");
    depends_on(&mut dependent, "CXC-B001");
    let s = ProjectState {
        tickets: vec![fixed_bug("CXC-B001"), dependent],
        ..ProjectState::default()
    };
    let a = assemble(&s);
    assert!(a.bundles.is_empty(), "no broken partial scope is assembled");
    assert_eq!(a.blocked.len(), 1);
    assert_eq!(a.blocked[0].ticket, tid("CXC-B002"));
    assert!(
        a.blocked[0].reason.contains("CXC-B001") && a.blocked[0].reason.contains("Fixed"),
        "the reason names the prerequisite and its status: {}",
        a.blocked[0].reason
    );
}

// ---------------------------------------------------------------------------
// AC#4 — removing one ticket affects only candidates depending on it
// ---------------------------------------------------------------------------

#[test]
fn ac4_removing_one_ticket_leaves_unrelated_bundles_unchanged() {
    let mut s = ProjectState {
        tickets: vec![
            verified_bug("CXC-B001"),
            verified_bug("CXC-B002"),
            verified_bug("CXC-B003"),
        ],
        ..ProjectState::default()
    };
    tag_goal(&mut s, 0, "bundle one");
    tag_goal(&mut s, 1, "bundle two");
    let before = assemble(&s);
    assert_eq!(before.bundles.len(), 3);

    // Remove bundle two's only ticket: bundle one and Unattributed are
    // byte-for-byte unchanged; nothing else re-evaluates.
    let mut s = ProjectState {
        tickets: vec![verified_bug("CXC-B001"), verified_bug("CXC-B003")],
        ..ProjectState::default()
    };
    tag_goal(&mut s, 0, "bundle one");
    let after = assemble(&s);

    assert_eq!(after.bundles.len(), 2);
    assert_eq!(bundle_named(&after, "G001"), bundle_named(&before, "G001"));
    assert_eq!(
        bundle_named(&after, "Unattributed"),
        bundle_named(&before, "Unattributed")
    );
}

// ---------------------------------------------------------------------------
// AC#5 — zero Verified-complete tickets: explicit empty state, no candidate
// ---------------------------------------------------------------------------

#[test]
fn ac5_zero_verified_complete_tickets_produce_an_explicit_empty_state() {
    let s = ProjectState {
        tickets: vec![fixed_bug("CXC-B001"), bug("CXC-B002")],
        ..ProjectState::default()
    };
    let a = assemble(&s);
    assert!(a.is_empty(), "the empty state is explicit, not an error");
    assert!(rc_members(&s).is_empty());

    let empty = ProjectState::default();
    assert!(assemble(&empty).is_empty());
}

// ---------------------------------------------------------------------------
// The cut-side manifest gate — honest-by-default subject correlation
// ---------------------------------------------------------------------------

#[test]
fn the_manifest_gate_is_honest_by_default_over_subjects() {
    assert_eq!(
        extract_ticket_refs("feat(cxa): wire REST store #CXA-F228"),
        Some(vec!["CXA-F228".to_owned()]),
        "conventional subjects carry their ticket ref"
    );
    assert_eq!(extract_ticket_refs("Merge branch 'x'"), None);
    assert_eq!(extract_ticket_refs("release: v2.27.0"), None);

    let subjects: Vec<String> = [
        "feat(cxa): wire REST store #CXA-F228", // verified → in
        "fix(cxa): partial support #CXA-B002",  // unverified → out
        "chore: no ticket ref at all",          // ref-less → out, never guessed
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let verified: BTreeSet<String> = ["CXA-F228"].iter().map(|s| (*s).to_owned()).collect();
    let m = filter_verified_subjects(&subjects, &verified);
    assert_eq!(m.included, [subjects[0].clone()]);
    assert_eq!(m.included_ids, ["CXA-F228"]);
    assert_eq!(m.excluded, [subjects[1].clone(), subjects[2].clone()]);
    assert_eq!(m.excluded_ids, ["CXA-B002"]);
}

#[test]
fn the_config_knob_defaults_to_the_verification_gate_on() {
    // Default: the gate is ON (documented-and-true, not a derived zero).
    let cfg = ReleasesConfig::default();
    assert!(cfg.cut_only_verified);
    // An old persisted document without the knob keeps the gate ON, and the
    // explicit opt-out is honored verbatim (migration path).
    let old: ReleasesConfig =
        serde_json::from_str(r#"{"enabled":true,"cut_every_days":7}"#).expect("old doc loads");
    assert!(old.cut_only_verified);
    let off: ReleasesConfig =
        serde_json::from_str(r#"{"enabled":true,"cut_every_days":7,"cut_only_verified":false}"#)
            .expect("opt-out loads");
    assert!(!off.cut_only_verified);
}

// ---------------------------------------------------------------------------
// The predicate is the single source of truth, shared with selection
// ---------------------------------------------------------------------------

#[test]
fn verified_complete_is_exactly_what_selection_treats_as_satisfied() {
    // The SA ruling pins the manifest's "complete" set to the SAME status set
    // selection::deps_satisfied accepts as a resolved dependency. The two
    // predicates must not drift, so this is checked through the PUBLIC
    // selection surface: a Ready feature whose only dependency is X is
    // selectable exactly when is_verified_complete(X) holds — for terminal
    // and non-terminal statuses of both lifecycles. Every probed dependency
    // sits at a non-Ready status, so the dependency itself is never a
    // ready-feature candidate and cannot pollute the answer.
    let check = |make: fn(&str) -> Ticket, id: &str, complete: bool| {
        let t = make(id);
        let mut dependent = ready_feature("CXC-F900");
        depends_on(&mut dependent, t.id().as_str());
        let s = ProjectState {
            tickets: vec![dependent, t],
            ..ProjectState::default()
        };
        assert_eq!(
            selection::next_ready_feature(&s).is_some(),
            complete,
            "selection disagrees with is_verified_complete for {id}"
        );
        assert_eq!(is_verified_complete(&s.tickets[1]), complete);
    };
    check(verified_bug, "CXC-B001", true);
    check(fixed_bug, "CXC-B002", false);
    check(bug, "CXC-B003", false);
    check(done_feature, "CXC-F010", true);
    check(documented_feature, "CXC-F011", true);
}
