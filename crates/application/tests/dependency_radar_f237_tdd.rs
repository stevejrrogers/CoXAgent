//! TDD tests for CXA-F237 — Dependency-aware blocked-ticket radar & critical-path
//! surface.
//!
//! Written before implementation so the ticket's acceptance criteria are pinned
//! as executable tests over the state/domain types the codebase has today.
//! Every fixture is built through the legal domain aggregate API only —
//! no server, no host harness, no network port, no fabricated persisted shape.
//!
//! The radar itself lives in `coxagent_application::dependency_radar` — a pure
//! derivation over `ProjectState` (the application layer is its home — it feeds
//! the ticket-detail payload, the backlog badge data and the dependencies
//! route). These tests bind that module to the ACs: each assertion below pins
//! one acceptance criterion against the production derivation, and the two
//! GREEN guards pin the laws the radar must agree with.
//!
//! AC → test map:
//! - AC1 (the ticket-detail surface for FEAT-B reports it as blocked by FEAT-A
//!   with FEAT-A's live status shown):
//!   [`ac1_detail_surface_reports_blocked_by_with_the_blockers_live_status`]
//! - AC2 (a purely cyclic pair A depends B / B depends A renders in the graph
//!   without hanging or recursing infinitely, both flagged with a cycle marker):
//!   [`ac2_cyclic_pair_is_flagged_on_both_members_and_the_graph_terminates`]
//! - AC3 (a Ready feature whose depends_on references an id absent from the
//!   project state is surfaced as 'unknown dependency', NOT treated as
//!   satisfied): [`ac3_unknown_dependency_is_surfaced_and_never_treated_as_satisfied`]
//! - AC4 (the backlog view renders a BLOCKED badge with the full blocking chain
//!   for any Ready ticket whose dependencies are not all Done):
//!   [`ac4_ready_ticket_with_unfinished_dependencies_reports_its_full_blocking_chain`]
//! - AC5 (`/api/projects/:pid/dependencies` returns nodes/edges derived only
//!   from ProjectState such that every edge corresponds to an actual TicketId
//!   in some ticket's depends_on):
//!   [`ac5_graph_nodes_and_edges_are_derived_only_from_declared_depends_on`]
//!
//! Two GREEN guards pin the laws the radar must agree with:
//! - [`guard_the_radar_never_disagrees_with_the_dev_gate_selection`] — the DEV
//!   gate already refuses a Ready feature whose dependencies are not all
//!   shipped-complete and releases it the moment they are; the radar's report
//!   must track the same live statuses, or the surfaces lie to the team.
//! - [`guard_the_write_boundary_refuses_the_states_the_radar_must_render`] —
//!   `ProjectState::validate` REJECTS both cyclic and dangling-dependency state
//!   at every persisted write. The AC2/AC3 fixtures are therefore in-memory
//!   states; the radar observes them as a total, cycle-safe derivation
//!   (restore/preview paths), which is exactly what the SA design asks for.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::selection;
use coxagent_application::state::ProjectState;
use coxagent_domain::{
    Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Fixtures — built ONLY through the domain aggregate's legal API.
// ---------------------------------------------------------------------------

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

/// A feature walked to `status` along the only legal edges, declaring `deps`
/// via the SA's `add_dependency` authority. Features start `Pending`
/// (`Ticket::new`); `Ready` needs the technical design; `InProgress`/`Done`
/// are DEV-FEATURE's edges.
fn feature_at(id: &str, status: Status, deps: &[&str]) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Feature,
        format!("feature {id}"),
        "fixture",
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("ticket");
    for dep in deps {
        t.add_dependency(Role::Sa, tid(dep))
            .expect("SA declares the dependency");
    }
    if status != Status::Pending {
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("attach design");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        if status != Status::Ready {
            for step in [Status::InProgress, Status::Done] {
                t.transition_to(Role::DevFeature, step).expect("walk");
                if step == status {
                    break;
                }
            }
        }
    }
    t
}

fn state_with(tickets: Vec<Ticket>) -> ProjectState {
    ProjectState {
        tickets,
        ..ProjectState::default()
    }
}

// ---------------------------------------------------------------------------
// The radar contract CXA-F237 implements — bound to the production module.
//
// The five pure derivations over `ProjectState` (and nothing else) live in
// `coxagent_application::dependency_radar`; the names below are the wire the
// ACs were pinned against.
// ---------------------------------------------------------------------------

use coxagent_application::dependency_radar::{
    blocked_by, blocking_chain, cycle_members, dependency_graph, unknown_dependencies,
    Blocker as BlockedTicket,
};

// --- AC1: the ticket-detail surface for FEAT-B reports it as blocked by
//     FEAT-A with FEAT-A's live status shown. ---
//
// RED: `blocked_by` is the missing radar derivation.
#[test]
fn ac1_detail_surface_reports_blocked_by_with_the_blockers_live_status() {
    // The AC's given: two features, FEAT-B depends_on FEAT-A, and FEAT-A has
    // moved past Ready into InProgress while FEAT-B is still Ready.
    let state = state_with(vec![
        feature_at("FEAT-A", Status::InProgress, &[]),
        feature_at("FEAT-B", Status::Ready, &["FEAT-A"]),
    ]);

    let blocked = blocked_by(&state, &tid("FEAT-B"));
    assert_eq!(
        blocked,
        vec![BlockedTicket {
            ticket: tid("FEAT-A"),
            status: Status::InProgress,
        }],
        "AC1: FEAT-B's detail surface must report it blocked by FEAT-A, with \
         FEAT-A's live status (InProgress) shown, not just its id"
    );

    // "Live" means the report tracks the blocker as it moves: once FEAT-A
    // reaches Done the same surface must report FEAT-B unblocked. The walk is
    // the legal InProgress -> Done edge, nothing staged.
    let mut live = state;
    let a = live.ticket_mut(&tid("FEAT-A")).expect("FEAT-A present");
    a.transition_to(Role::DevFeature, Status::Done)
        .expect("InProgress -> Done is a legal feature edge for DEV-FEATURE");
    assert!(
        blocked_by(&live, &tid("FEAT-B")).is_empty(),
        "AC1: the live status is the point — a blocker that shipped no longer \
         blocks, and the surface must say so"
    );
}

// --- AC2: a purely cyclic pair A depends B and B depends A renders in the
//     graph without hanging or recursing infinitely, both flagged with a
//     cycle marker. ---
//
// RED: `cycle_members` and `dependency_graph` are the missing radar
// derivations. A test that hangs fails the run by timeout — that IS the
// no-hang clause being enforced.
#[test]
fn ac2_cyclic_pair_is_flagged_on_both_members_and_the_graph_terminates() {
    let state = state_with(vec![
        feature_at("FEAT-A", Status::Ready, &["FEAT-B"]),
        feature_at("FEAT-B", Status::Ready, &["FEAT-A"]),
    ]);

    let members = cycle_members(&state);
    assert_eq!(
        members,
        BTreeSet::from([tid("FEAT-A"), tid("FEAT-B")]),
        "AC2: BOTH members of the pure cycle carry the cycle marker — flagging \
         only the first one found is not the AC"
    );

    // "Renders in the graph without hanging or recursing infinitely": the
    // graph derivation over the cyclic state must RETURN, keep both members
    // in it (a cycle is dropped work, not invisible work), and fabricate no
    // edges beyond the two declared ones.
    let (nodes, edges) = dependency_graph(&state);
    let node_ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        node_ids,
        BTreeSet::from(["FEAT-A", "FEAT-B"]),
        "AC2: the cyclic pair renders in the graph — both members present"
    );
    let mut edge_pairs: Vec<(&str, &str)> = edges
        .iter()
        .map(|e| (e.dependent.as_str(), e.prerequisite.as_str()))
        .collect();
    edge_pairs.sort_unstable();
    assert_eq!(
        edge_pairs,
        vec![("FEAT-A", "FEAT-B"), ("FEAT-B", "FEAT-A")],
        "AC2: exactly the two declared cycle edges, nothing fabricated"
    );

    // The chain walk over a cycle member must terminate with a finite answer
    // (visited-set discipline), not recurse forever.
    let chain = blocking_chain(&state, &tid("FEAT-A"));
    assert!(
        chain.contains(&tid("FEAT-B")),
        "AC2: the chain derivation over a cycle terminates and still sees the \
         other member: {chain:?}"
    );
}

// --- AC3: a Ready feature whose depends_on references an id absent from the
//     project state is surfaced as 'unknown dependency', NOT treated as
//     satisfied. ---
//
// RED on the surfacing half (`unknown_dependencies`); the not-satisfied half
// is asserted through the existing DEV gate, which already complies.
#[test]
fn ac3_unknown_dependency_is_surfaced_and_never_treated_as_satisfied() {
    // FEAT-ZZZ is nowhere in the project state; FEAT-B declares it anyway.
    let state = state_with(vec![feature_at("FEAT-B", Status::Ready, &["FEAT-ZZZ"])]);

    // Surfacing: the radar names the unknown dependency explicitly.
    assert!(
        unknown_dependencies(&state).contains(&(tid("FEAT-B"), tid("FEAT-ZZZ"))),
        "AC3: a depends_on id absent from project state is surfaced as an \
         'unknown dependency', not silently dropped"
    );
    // NOT satisfied: the unknown id must not read as a shipped prerequisite.
    // Driven through the existing DEV gate — today `selection` already
    // refuses the ticket; the radar must agree (never call it satisfied).
    assert_ne!(
        selection::next_ready_feature(&state),
        Some(tid("FEAT-B")),
        "AC3: an unknown dependency is NOT treated as satisfied — FEAT-B stays \
         out of the DEV queue"
    );
    // And the known-blocker report stays empty: an unknown id is not a ticket
    // with a status, it is the separate 'unknown dependency' surface.
    assert!(
        blocked_by(&state, &tid("FEAT-B")).is_empty(),
        "AC3: the unknown id has no live status to show — it must not be \
         reported as a known blocker"
    );
}

// --- AC4: the backlog view renders a BLOCKED badge with the full blocking
//     chain for any Ready ticket whose dependencies are not all Done. ---
//
// RED: `blocking_chain` is the missing radar derivation. The badge itself is
// the backlog view's rendering of exactly this data (ASK SA #1 names the wire
// shape); the pure substance the badge must carry is pinned here.
#[test]
fn ac4_ready_ticket_with_unfinished_dependencies_reports_its_full_blocking_chain() {
    // FEAT-C is the root blocker (InProgress), FEAT-A is Ready behind it,
    // FEAT-B is Ready behind FEAT-A — a two-link chain.
    let state = state_with(vec![
        feature_at("FEAT-C", Status::InProgress, &[]),
        feature_at("FEAT-A", Status::Ready, &["FEAT-C"]),
        feature_at("FEAT-B", Status::Ready, &["FEAT-A"]),
    ]);

    assert_eq!(
        blocking_chain(&state, &tid("FEAT-B")),
        vec![tid("FEAT-A"), tid("FEAT-C")],
        "AC4: the BLOCKED badge carries the FULL chain — the direct blocker \
         AND the blocker behind it, prerequisites-first, not just the direct \
         depends_on entries"
    );
    assert_eq!(
        blocking_chain(&state, &tid("FEAT-A")),
        vec![tid("FEAT-C")],
        "AC4: every Ready ticket with unfinished dependencies gets its own \
         chain — FEAT-A's badge is its own, not FEAT-B's"
    );
    // A ticket whose dependencies ARE all shipped-complete has an empty
    // chain: the badge is for blocked work only, never decoration.
    let released = state_with(vec![
        feature_at("FEAT-C", Status::Done, &[]),
        feature_at("FEAT-A", Status::Ready, &["FEAT-C"]),
    ]);
    assert!(
        blocking_chain(&released, &tid("FEAT-A")).is_empty(),
        "AC4: a dependency that reached Done releases its dependent — no \
         BLOCKED badge for unblocked work"
    );
}

// --- AC5: /api/projects/:pid/dependencies returns nodes/edges derived only
//     from ProjectState such that every edge corresponds to an actual
//     TicketId in some ticket's depends_on. ---
//
// RED: `dependency_graph` is the missing radar derivation. The route itself
// is the presentation binding of exactly this derivation (ASK SA #1); the
// pure contract the route must serve is pinned here.
#[test]
fn ac5_graph_nodes_and_edges_are_derived_only_from_declared_depends_on() {
    let state = state_with(vec![
        feature_at("FEAT-C", Status::InProgress, &[]),
        feature_at("FEAT-A", Status::Ready, &["FEAT-C"]),
        feature_at("FEAT-B", Status::Ready, &["FEAT-A"]),
    ]);

    let (nodes, edges) = dependency_graph(&state);
    let node_ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        node_ids,
        BTreeSet::from(["FEAT-A", "FEAT-B", "FEAT-C"]),
        "AC5: nodes are derived only from ProjectState — exactly the tickets \
         in state, no phantoms"
    );
    let mut edge_pairs: Vec<(&str, &str)> = edges
        .iter()
        .map(|e| (e.dependent.as_str(), e.prerequisite.as_str()))
        .collect();
    edge_pairs.sort_unstable();
    assert_eq!(
        edge_pairs,
        vec![("FEAT-A", "FEAT-C"), ("FEAT-B", "FEAT-A")],
        "AC5: every edge corresponds to an actual TicketId in some ticket's \
         depends_on — and nothing else is invented (no transitive FEAT-B -> \
         FEAT-C shortcut, no goal or parent edges)"
    );

    // "Derived ONLY from ProjectState" cuts both ways: a declared depends_on
    // entry whose target is absent STILL yields its edge (it is an actual
    // entry in some ticket's depends_on) but must NOT yield a node — the
    // unknown target is AC3's surface, not a graph node.
    let dangling = state_with(vec![
        feature_at("FEAT-A", Status::Ready, &[]),
        feature_at("FEAT-B", Status::Ready, &["FEAT-A", "FEAT-ZZZ"]),
    ]);
    let (nodes, edges) = dependency_graph(&dangling);
    let node_ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        node_ids,
        BTreeSet::from(["FEAT-A", "FEAT-B"]),
        "AC5: no phantom node for an id the project state does not have"
    );
    let mut edge_pairs: Vec<(&str, &str)> = edges
        .iter()
        .map(|e| (e.dependent.as_str(), e.prerequisite.as_str()))
        .collect();
    edge_pairs.sort_unstable();
    assert_eq!(
        edge_pairs,
        vec![("FEAT-B", "FEAT-A"), ("FEAT-B", "FEAT-ZZZ")],
        "AC5: the declared edge to the absent id is served — every edge \
         corresponds to an actual depends_on entry"
    );
}

// ---------------------------------------------------------------------------
// GREEN guards — the laws the radar must agree with. They pass today (the
// F030 pattern: pinned invariants the implementation must keep).
// ---------------------------------------------------------------------------

/// The DEV gate already refuses a Ready feature whose dependencies are not
/// all shipped-complete and releases it the moment they are. The radar's
/// blocked-by report (AC1) and the DEV queue are two views of ONE fact: if
/// they ever disagree, either the backlog badges work DEV cannot take, or DEV
/// starves behind work the backlog calls unblocked.
#[test]
fn guard_the_radar_never_disagrees_with_the_dev_gate_selection() {
    let mut state = state_with(vec![
        feature_at("FEAT-A", Status::InProgress, &[]),
        feature_at("FEAT-B", Status::Ready, &["FEAT-A"]),
    ]);
    assert_eq!(
        selection::next_ready_feature(&state),
        None,
        "the DEV gate refuses FEAT-B while FEAT-A is InProgress — the radar \
         must report the same block"
    );
    let a = state.ticket_mut(&tid("FEAT-A")).expect("FEAT-A present");
    a.transition_to(Role::DevFeature, Status::Done)
        .expect("InProgress -> Done is a legal feature edge for DEV-FEATURE");
    assert_eq!(
        selection::next_ready_feature(&state),
        Some(tid("FEAT-B")),
        "the DEV gate releases FEAT-B once FEAT-A reaches Done — the radar's \
         live-status report must flip with it"
    );
}

/// `ProjectState::validate` REJECTS, at every persisted write, exactly the
/// two states AC2 and AC3 describe rendering: a dependency cycle, and a
/// `depends_on` id absent from the project state. Pinned so the F237 design
/// (ASK SA #2) is made against the real law: today these states exist only
/// in memory / in restore-import paths — the radar's cycle and unknown-dep
/// surfaces must say where they observe such state from.
#[test]
fn guard_the_write_boundary_refuses_the_states_the_radar_must_render() {
    let cyclic = state_with(vec![
        feature_at("FEAT-A", Status::Ready, &["FEAT-B"]),
        feature_at("FEAT-B", Status::Ready, &["FEAT-A"]),
    ]);
    let err = cyclic
        .validate()
        .expect_err("the write-boundary law refuses a dependency cycle");
    assert!(
        err.contains("cycle"),
        "the existing law names the cycle: {err}"
    );

    let dangling = state_with(vec![feature_at("FEAT-B", Status::Ready, &["FEAT-ZZZ"])]);
    let err = dangling
        .validate()
        .expect_err("the write-boundary law refuses a dangling dependency");
    assert!(
        err.contains("unknown ticket"),
        "the existing law names the unknown ticket: {err}"
    );
}
