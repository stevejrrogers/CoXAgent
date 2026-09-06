//! TDD tests for CXA-F254 — the cross-project duplicate radar's AC4
//! exemption: exact-match shared-infrastructure tickets carrying the same
//! bounded-context service tag are NOT reported as duplicates unless their
//! dissimilarity exceeds the stricter bound
//! ([`coxagent_application::parsing::SAME_TAG_MAX_DISSIMILARITY`]).
//!
//! Written against the state/domain types the codebase has today. Every
//! fixture is built through the REAL domain aggregate API — a `ProjectState`
//! holding `Ticket`s whose service tag is stamped with the BA's own
//! `set_service_tag` authority — then mapped to radar snapshots exactly the
//! way the HTTP adapter maps them. No server, no host harness, no network
//! port, no fabricated persisted shape: the pure decision core
//! (`find_cross_project_duplicates`) is the subject under test.
//!
//! AC → test map:
//! - AC4 (exact same-tag matches across two projects are exempt, dissimilarity 0):
//!   [`ac4_exact_same_tag_proposals_across_two_projects_are_exempt`]
//! - AC4 (the bound is inclusive: dissimilarity exactly at the bound stays exempt):
//!   [`ac4_the_dissimilarity_bound_is_inclusive_boundary_pairs_stay_exempt`]
//! - AC4 (once dissimilarity EXCEEDS the bound the pair surfaces — flagged,
//!   never auto-blocked — even between same-tag tickets):
//!   [`ac4_same_tag_paraphrase_past_the_bound_still_surfaces_for_a_human`]
//! - AC4 (an absent tag is no exemption — the carve-out is the tag's doing):
//!   [`ac4_an_absent_tag_is_no_exemption`]
//! - AC3 (the persisted allowlist still excludes a same-tag pair the human
//!   resolved once it surfaced past the bound):
//!   [`ac3_the_allowlist_still_excludes_a_reported_same_tag_pair`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::state::ProjectState;
use coxagent_application::use_cases::duplicate_radar::{
    find_cross_project_duplicates, pair_key, TicketSnapshot,
};
use coxagent_domain::{Complexity, Priority, Role, Ticket, TicketId, TicketType};

// ---------------------------------------------------------------------------
// Fixtures — built ONLY through the domain aggregate's legal API.
// ---------------------------------------------------------------------------

/// A pending feature proposal — the scenario CXA-F254 describes: each
/// project's BA files proposals against its own board — with an optional
/// bounded-context service tag stamped by the BA, the same authority the
/// real insert loop uses.
fn proposal(id: &str, title: &str, scope: &str, tag: Option<&str>) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Feature,
        title.to_owned(),
        scope.to_owned(),
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("valid ticket");
    if let Some(tag) = tag {
        t.set_service_tag(Role::Ba, tag)
            .expect("BA stamps the bounded-context service tag");
    }
    t
}

/// One registered project's backlog: a real `ProjectState` holding the
/// proposals.
fn backlog(tickets: &[Ticket]) -> ProjectState {
    ProjectState {
        tickets: tickets.to_vec(),
        ..ProjectState::default()
    }
}

/// The exact per-ticket mapping the HTTP adapter performs when it gathers
/// radar snapshots (title + scope metadata only, never raw documents).
/// Pending proposals sit inside the radar's active pool on both sides.
fn snapshots_for(
    project_id: &str,
    project_name: &str,
    state: &ProjectState,
) -> Vec<TicketSnapshot> {
    state
        .tickets
        .iter()
        .map(|t| TicketSnapshot {
            project_id: project_id.to_owned(),
            project_name: project_name.to_owned(),
            ticket_id: t.id().as_str().to_owned(),
            title: t.title().to_owned(),
            scope: t.description().trim().to_owned(),
            service_tag: t.service_tag().map(ToOwned::to_owned),
        })
        .collect()
}

/// The radar's view of the whole hub: Alpha and Beta, two registered projects
/// that each filed their own proposals.
fn fleet(alpha: &ProjectState, beta: &ProjectState) -> Vec<TicketSnapshot> {
    snapshots_for("p-alpha", "Alpha", alpha)
        .into_iter()
        .chain(snapshots_for("p-beta", "Beta", beta))
        .collect()
}

// ---------------------------------------------------------------------------
// AC4 — the same-service-tag exemption.
// ---------------------------------------------------------------------------

#[test]
fn ac4_exact_same_tag_proposals_across_two_projects_are_exempt() {
    // The same shared-infrastructure service, filed verbatim by two projects'
    // BAs, both tagged: dissimilarity 0 is always within the bound, so the
    // pair is presumed legitimate and never queues a human verdict.
    let alpha = backlog(&[proposal(
        "CXC-F101",
        "Add Redis cache layer",
        "shared session store",
        Some("redis-cache"),
    )]);
    let beta = backlog(&[proposal(
        "CXC-B101",
        "Add Redis cache layer",
        "shared session store",
        Some("redis-cache"),
    )]);
    assert!(
        find_cross_project_duplicates(&fleet(&alpha, &beta), &[]).is_empty(),
        "identical same-tag proposals are exempt, not reported"
    );
}

#[test]
fn ac4_the_dissimilarity_bound_is_inclusive_boundary_pairs_stay_exempt() {
    // One token apart (jaccard 0.8 ⇒ dissimilarity exactly the bound):
    // "within the bound" includes the boundary, so the pair stays exempt.
    let alpha = backlog(&[proposal(
        "CXC-F102",
        "Fix flaky login flow",
        "session drops",
        Some("auth"),
    )]);
    let beta = backlog(&[proposal(
        "CXC-B102",
        "Fix flaky login flow for mobile",
        "session drops on mobile",
        Some("auth"),
    )]);
    let snaps = fleet(&alpha, &beta);
    let score = coxagent_application::parsing::jaccard(
        &coxagent_application::parsing::title_tokens("Fix flaky login flow"),
        &coxagent_application::parsing::title_tokens("Fix flaky login flow for mobile"),
    );
    // The fixture must sit EXACTLY on the bound: dissimilarity 1 − 0.8 equals
    // the SA-pinned 0.2, so this pins inclusivity ("while dissimilarity is
    // within the bound"). A retuned bound fails here and forces the fixture
    // to be re-derived — deliberately.
    assert!(
        (1.0 - score - coxagent_application::parsing::SAME_TAG_MAX_DISSIMILARITY).abs() < 1e-12,
        "fixture dissimilarity must equal the bound exactly, got {score}"
    );
    assert!(
        find_cross_project_duplicates(&snaps, &[]).is_empty(),
        "a pair at the bound is within it, hence exempt"
    );
}

#[test]
fn ac4_same_tag_paraphrase_past_the_bound_still_surfaces_for_a_human() {
    // Same tag, but the wording drifted past the stricter bound (jaccard 0.75
    // ⇒ dissimilarity 0.25 > 0.2): the drift may mean the two tickets are NOT
    // the same shared service, so the pair still surfaces — flagged for an
    // explicit operator decision, never auto-blocked.
    let alpha = backlog(&[proposal(
        "CXC-F103",
        "Add Redis cache layer",
        "shared session store",
        Some("redis-cache"),
    )]);
    let beta = backlog(&[proposal(
        "CXC-B103",
        "Add Redis cache layer with TTL",
        "shared session store that expires",
        Some("redis-cache"),
    )]);
    let out = find_cross_project_duplicates(&fleet(&alpha, &beta), &[]);
    assert_eq!(out.len(), 1, "past the bound the pair surfaces again");
    assert_eq!(
        out[0].dups[0].score,
        Some(0.75),
        "the computed similarity travels with the pair"
    );
}

#[test]
fn ac4_an_absent_tag_is_no_exemption() {
    // The carve-out is the TAG's doing: the identical pair without tags is an
    // ordinary radar match (and a real duplicate risk).
    let alpha = backlog(&[proposal(
        "CXC-F104",
        "Add burn-trend dashboard",
        "burn trends over cycles",
        None,
    )]);
    let beta = backlog(&[proposal(
        "CXC-B104",
        "Add burn-trend dashboard",
        "burn trends over cycles",
        None,
    )]);
    let out = find_cross_project_duplicates(&fleet(&alpha, &beta), &[]);
    assert_eq!(out.len(), 1, "no tag, no exemption");
    assert_eq!(out[0].dups[0].score, Some(1.0));
}

#[test]
fn ac3_the_allowlist_still_excludes_a_reported_same_tag_pair() {
    // A same-tag pair past the bound surfaces; the operator's allow verdict
    // persists it in the allowlist and the radar never reports it again.
    let alpha = backlog(&[proposal(
        "CXC-F105",
        "Add Redis cache layer",
        "shared session store",
        Some("redis-cache"),
    )]);
    let beta = backlog(&[proposal(
        "CXC-B105",
        "Add Redis cache layer with TTL",
        "shared session store that expires",
        Some("redis-cache"),
    )]);
    let snaps = fleet(&alpha, &beta);
    assert_eq!(find_cross_project_duplicates(&snaps, &[]).len(), 1);
    let key = pair_key(("p-alpha", "CXC-F105"), ("p-beta", "CXC-B105"));
    assert!(
        find_cross_project_duplicates(&snaps, &[key]).is_empty(),
        "an allowed pair is excluded from future runs"
    );
}
