//! TDD tests for CXA-F361 — Roadmap depth: per-milestone progress bars with
//! linked-ticket drill-in. RED half: the progress derivation.
//!
//! The ticket's acceptance criterion encoded here, verbatim:
//! "Progress derivation is a pure function over (milestones, tickets) with
//! unit tests on fixtures."
//!
//! WHERE THE DERIVATION LIVES AND WHY: the application layer's milestone
//! read model (`coxagent_application::milestone_projection`) is the codebase's
//! pure function over the state's milestones and tickets — F249 made it the
//! per-milestone classification read, F252 added the only ticket↔milestone
//! link that exists anywhere (a ticket attributes to the first unreleased
//! milestone — the active target — iff it sits in the active sprint's
//! `committed` set; `attributed_milestone`). F361's per-milestone completion
//! percent is therefore derived on the SAME read model, from the SAME data —
//! no new persisted field, no fabricated link:
//! - a `released` row is complete by the release pipeline's own record: 100;
//! - the active row's committed scope: the share of its tickets whose status
//!   closes scope (`Done`/`Documented`/`Verified`/`Rejected` — the same
//!   settled set `closes_scope` accepts), rounded to an integer percent;
//! - a row with no attributed scope (future rows, Kanban, parse-broken or
//!   absent roadmaps) reads 0 — "no attributed work" must never divide into
//!   NaN and never masquerade as either done or unstarted-by-choice.
//!
//! Every fixture is built through the legal domain aggregate API and the real
//! persisted shapes only — no server, no host harness, no network port, no
//! fabricated persisted fields (the `milestone_projection_f249_tdd.rs`
//! discipline). The assertions ride the read model's SERIALIZED wire via
//! `serde_json` key access, so this file compiles against today's code and
//! fails only because the progress derivation is missing (the key reads
//! null). If the field lands under a different name, move the key with it
//! (the `preflight_f239_tdd.rs` convention).
//!
//! NOT ENCODED HERE: the view half (strip markup, collapse marker, click-to-
//! filter toggle) — pinned over the served view sources in
//! `crates/app/tests/roadmap_milestone_strip_f361_tdd.rs` — and the live
//! `cd e2e && npx playwright test` run with refreshed goldens (AC4's run
//! itself), verified against the running app in the QA phase.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::milestone_projection::projection_report;
use coxagent_application::state::{Milestone, ProjectState, Sprint};
use coxagent_domain::{
    Complexity, Priority, Role, SemVer, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};

// ---------------------------------------------------------------------------
// Fixtures — built ONLY through the domain aggregate's legal API (F249 style).
// ---------------------------------------------------------------------------

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

/// A feature walked to `status` along the only legal edges. Features start
/// `Pending` (`Ticket::new`); `Ready` needs the technical design;
/// `InProgress`/`Done` are DEV-FEATURE's edges.
fn feature_at(id: &str, status: Status) -> Ticket {
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
    if status != Status::Pending {
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("attach design");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        if matches!(status, Status::InProgress | Status::Done) {
            t.transition_to(Role::DevFeature, Status::InProgress)
                .expect("claim");
        }
        if status == Status::Done {
            t.transition_to(Role::DevFeature, Status::Done)
                .expect("done");
        }
    }
    t
}

fn milestone(name: &str, target: &str, goal_complete: bool, fulfilled: bool) -> Milestone {
    Milestone {
        name: name.to_owned(),
        goal: format!("the {name} goal"),
        target_version: target.to_owned(),
        goal_complete,
        fulfilled,
    }
}

fn state_with(current: &str, milestones: Vec<Milestone>) -> ProjectState {
    ProjectState {
        current_version: SemVer::parse(current).expect("version"),
        milestones,
        ..ProjectState::default()
    }
}

fn scrum(committed: &[&str]) -> Sprint {
    Sprint {
        number: 1,
        goal: String::new(),
        started_cycle: 0,
        length_cycles: 10,
        committed: committed
            .iter()
            .copied()
            .filter_map(|c| TicketId::new(c).ok())
            .collect(),
        started_at: String::new(),
        bug_burn_floor: None,
    }
}

/// The classic three-row roadmap the strip renders: a shipped row, the active
/// target the sprint's commit works toward, and a future row nobody committed
/// to yet.
fn shipped_active_future() -> ProjectState {
    let mut state = state_with(
        "0.5.2",
        vec![
            milestone("Shipped", "0.5.0", true, true),
            milestone("Beta", "0.9.0", false, false),
            milestone("GA", "1.0.0", false, false),
        ],
    );
    state.tickets = vec![
        feature_at("FEAT-DONE", Status::Done),
        feature_at("FEAT-PROG", Status::InProgress),
        feature_at("FEAT-READY", Status::Ready),
    ];
    state.sprint = Some(scrum(&["FEAT-DONE", "FEAT-PROG", "FEAT-READY"]));
    state
}

// ---------------------------------------------------------------------------
// AC3 — per-milestone progress derived from ticket completion.
// ---------------------------------------------------------------------------

/// AC3: "progress bar with % derived from ticket completion" — every row of
/// the read model carries its completion percent, derived purely from the
/// tickets attributed to it:
/// - `Shipped` is fulfilled → 100, by the release pipeline's own record, even
///   though attribution (F252) correctly parks no scope on a released row;
/// - `Beta` (the active target) owns the sprint's three committed tickets, of
///   which exactly one has closed scope (`Done`) → 1 of 3 → 33%;
/// - `GA` has no attributed scope → 0.
///
/// RED: no progress key exists on the read model's wire today (F249/F252
/// pinned the exact key set), so every lookup reads null.
#[test]
fn ac3_progress_is_derived_per_milestone_from_ticket_completion() {
    let state = shipped_active_future();
    let v = serde_json::to_value(projection_report(&state)).expect("serialize");

    let rows = v["milestones"].as_array().expect("milestone rows");
    assert_eq!(rows.len(), 3, "one progress figure per milestone row");

    assert_eq!(
        rows[0]["progress"].as_f64(),
        Some(100.0),
        "a released milestone is complete by the pipeline's own record — \
         with zero attributed scope it must still read 100, never 0: {}",
        rows[0]
    );
    assert_eq!(
        rows[1]["progress"].as_f64(),
        Some(33.0),
        "the active target's bar is its committed scope's closed share: \
         1 of 3 tickets closed (Done) → 33% — got {}",
        rows[1]
    );
    assert_eq!(
        rows[2]["progress"].as_f64(),
        Some(0.0),
        "a future milestone has no attributed tickets — its bar reads 0: {}",
        rows[2]
    );
}

/// AC3: the conservative edges — "no attributed scope" is 0%, never a lie:
/// - Kanban (no sprint, no committed set): the active row owns nothing, so
///   the empty scope must divide to 0 — never NaN, never 100;
/// - a parse-broken roadmap (F252): attribution withholds scope the release
///   pipeline could never complete, so the broken active row reads 0 too.
///
/// RED: the progress key does not exist, so both edges read null today.
#[test]
fn ac3_progress_is_zero_never_a_lie_when_no_scope_is_attributed() {
    // Kanban: an active roadmap with tickets on the board but no sprint.
    let kanban = state_with("0.5.2", vec![milestone("Beta", "0.9.0", false, false)]);
    let v = serde_json::to_value(projection_report(&kanban)).expect("serialize");
    assert_eq!(
        v["milestones"][0]["progress"].as_f64(),
        Some(0.0),
        "an empty committed scope reads 0 — not NaN, not 100: {}",
        v["milestones"][0]
    );

    // Parse-broken active target: scope is withheld (CXA-F252), so the bar
    // has nothing to measure and reads 0 rather than inventing a figure.
    let broken = ProjectState {
        tickets: vec![feature_at("FEAT-A", Status::InProgress)],
        sprint: Some(scrum(&["FEAT-A"])),
        ..state_with("0.5.2", vec![milestone("Broken", "soon", false, false)])
    };
    let v = serde_json::to_value(projection_report(&broken)).expect("serialize");
    assert_eq!(
        v["milestones"][0]["progress"].as_f64(),
        Some(0.0),
        "a parse-broken roadmap attributes nothing, so its bar reads 0: {}",
        v["milestones"][0]
    );
}

/// AC3: the derivation is a PURE function of the state it reads — the same
/// snapshot always yields the same figures, the read never mutates the state,
/// and the figure MOVES when the tickets' completion moves (a committed
/// ticket closing scope moves the bar; every committed ticket closed reads
/// 100). The purity half is a green premise; the movement half is RED (the
/// progress key does not exist yet).
#[test]
fn ac3_progress_tracks_ticket_completion_as_state_changes() {
    // 2 of 4 committed tickets closed → 50%.
    let mut state = state_with("0.5.2", vec![milestone("Beta", "0.9.0", false, false)]);
    state.tickets = vec![
        feature_at("FEAT-A", Status::Done),
        feature_at("FEAT-B", Status::Done),
        feature_at("FEAT-C", Status::InProgress),
        feature_at("FEAT-D", Status::Ready),
    ];
    state.sprint = Some(scrum(&["FEAT-A", "FEAT-B", "FEAT-C", "FEAT-D"]));
    let before = state.clone();

    let first = projection_report(&state);
    assert_eq!(state, before, "the read never mutates the state it reads");
    assert_eq!(
        projection_report(&state),
        first,
        "the same snapshot always yields the same report — no IO, no drift"
    );
    let v = serde_json::to_value(&first).expect("serialize");
    assert_eq!(
        v["milestones"][0]["progress"].as_f64(),
        Some(50.0),
        "2 of 4 committed tickets closed → 50% — got {}",
        v["milestones"][0]
    );

    // The remaining two close scope → the same pure read now reports 100.
    let mut done = state.clone();
    for t in &mut done.tickets {
        if t.status() == Status::InProgress || t.status() == Status::Ready {
            if t.status() == Status::Ready {
                t.transition_to(Role::DevFeature, Status::InProgress)
                    .expect("claim");
            }
            t.transition_to(Role::DevFeature, Status::Done)
                .expect("done");
        }
    }
    let v = serde_json::to_value(projection_report(&done)).expect("serialize");
    assert_eq!(
        v["milestones"][0]["progress"].as_f64(),
        Some(100.0),
        "every committed ticket closed → the active bar reads 100: {}",
        v["milestones"][0]
    );
}

// ---------------------------------------------------------------------------
// Green premise — the strip's drill-in and collapse render from data the
// read model already carries (CXA-F252). The view half of the ticket filters
// buckets by the active row's `committed` list and collapses rows on
// `released`; if either field ever leaves the wire, the strip renders from
// nothing — fail HERE with the surface named, not silently in a screenshot.
// ---------------------------------------------------------------------------

#[test]
fn the_strip_data_the_drill_in_and_collapse_render_from_is_on_the_wire() {
    let state = shipped_active_future();
    let v = serde_json::to_value(projection_report(&state)).expect("serialize");
    let row = &v["milestones"][1];

    assert_eq!(row["name"], "Beta", "the strip labels each row by name");
    assert_eq!(
        row["target_version"], "0.9.0",
        "the target-version chip renders from the row's own target"
    );
    assert_eq!(
        row["released"], false,
        "the active row is not complete — the collapse check stays off it"
    );
    assert_eq!(
        v["milestones"][0]["released"], true,
        "the shipped row flips the very flag the collapse keys on"
    );
    assert_eq!(
        row["committed"],
        serde_json::json!(["FEAT-DONE", "FEAT-PROG", "FEAT-READY"]),
        "the drill-in/filter basis — the active row's attributed ticket ids, \
         in commit order — is on the wire (CXA-F252)"
    );
}
