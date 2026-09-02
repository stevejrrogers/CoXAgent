//! TDD tests for CXA-F329 — Slot collision radar: warn at claim time when two
//! claimed tickets will touch the same files.
//!
//! Written in the CXA-F237 style: executable ACs over the state/domain types
//! the codebase has today, fixtures built ONLY through the legal domain
//! aggregate API — no server, no host harness, no network port, no fabricated
//! persisted shape. The radar itself lives in
//! `coxagent_application::slot_collision_radar` — a pure derivation over
//! `ProjectState` (the application layer is its home: it feeds the claim-time
//! brief block, the board badge data and the snapshot's `derived.collisions`).
//!
//! AC → test map:
//! - AC1 (a claim succeeds while another slot holds a claim predicted to
//!   touch overlapping files, and the collision surfaces on the board and in
//!   the claiming agent's brief):
//!   [`ac1_the_colliding_pair_is_reported_with_partner_and_files`],
//!   [`ac1_the_wire_shape_carries_pairs_for_the_board_badge`],
//!   [`guard_the_radar_is_advisory_only_and_never_touches_the_dev_gate`]
//! - AC2 (two claims predicted to touch disjoint files produce no warning):
//!   [`ac2_disjoint_declared_files_stay_silent`]
//! - AC3 (a ticket with no explicit file hints is visibly marked
//!   low-confidence rather than silently skipped):
//!   [`ac3_a_ticket_without_file_hints_is_radar_blind_marked_low_confidence`]
//! - AC4 (no usable repo map degrades to no warning, never fails a claim):
//!   [`ac4_a_state_without_usable_designs_degrades_and_never_fails`]
//! - AC5 (warnings never fire across projects):
//!   [`ac5_the_radar_only_sees_the_one_project_state_it_is_handed`]
//!
//! GREEN guard pins the law the radar must agree with:
//! - [`guard_the_radar_is_advisory_only_and_never_touches_the_dev_gate`] —
//!   the radar is a read-only derivation rendered AFTER the claim succeeded;
//!   the DEV gate's selection must be byte-identical whether or not running
//!   slots collide, or an advisory would become a veto.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::selection;
use coxagent_application::slot_collision_radar::{claim_warning, collision_radar, collisions_for};
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

/// A feature walked to `status` along the only legal edges, declaring `files`
/// in the SA's technical design. Features start `Pending`; `Ready` needs the
/// design; `InProgress`/`Done` are DEV-FEATURE's edges.
fn feature_at(id: &str, status: Status, files: &[&str]) -> Ticket {
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
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            files: files.iter().map(|f| (*f).to_owned()).collect(),
            ..TechnicalDesign::default()
        },
    )
    .expect("attach design");
    if status != Status::Pending {
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

/// A bug walked `Open -> InProgress` (the legal DEV-BUG edge), declaring
/// `files`. The design's own example pairs a B-ticket into `unknown_files`.
fn bug_at(id: &str, status: Status, files: &[&str]) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Bug,
        format!("bug {id}"),
        "fixture",
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("ticket");
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            files: files.iter().map(|f| (*f).to_owned()).collect(),
            ..TechnicalDesign::default()
        },
    )
    .expect("attach design");
    if status == Status::InProgress {
        t.transition_to(Role::DevBug, Status::InProgress)
            .expect("walk");
    }
    t
}

fn state_with(tickets: Vec<Ticket>) -> ProjectState {
    ProjectState {
        tickets,
        ..ProjectState::default()
    }
}

// --- AC1: a claim succeeds while another slot runs a colliding ticket, and
//     the collision surfaces with the partner id and the shared files. ---

#[test]
fn ac1_the_colliding_pair_is_reported_with_partner_and_files() {
    // The AC's given: FEAT-B is claimed (InProgress) while FEAT-A runs in
    // another slot; both designs declare the same file.
    let state = state_with(vec![
        feature_at("FEAT-A", Status::InProgress, &["crates/app/src/main.rs"]),
        feature_at("FEAT-B", Status::InProgress, &["crates/app/src/main.rs"]),
    ]);

    // The claim-time view: the claiming candidate's radar names the partner
    // and the shared files — that block lands in the agent's brief.
    let for_candidate = collisions_for(&state, &tid("FEAT-B"));
    assert_eq!(
        for_candidate,
        vec![coxagent_application::slot_collision_radar::CollisionPair {
            a: tid("FEAT-B"),
            b: tid("FEAT-A"),
            files: vec!["crates/app/src/main.rs".to_owned()],
        }],
        "AC1: the candidate is paired against the OTHER running slot with the \
         exact shared file"
    );
    let warning = claim_warning(&state, &tid("FEAT-B"));
    assert!(
        warning.contains("FEAT-A") && warning.contains("crates/app/src/main.rs"),
        "AC1: the brief block names the overlapping ticket and the shared files: {warning}"
    );

    // The board view: the whole-state radar reports the same pair exactly
    // once, ids lexicographically sorted within the pair.
    let pairs = collision_radar(&state).pairs;
    assert_eq!(pairs.len(), 1, "AC1: one symmetric pair, not two");
    assert_eq!(pairs[0].a.as_str(), "FEAT-A", "AC1: a < b within the pair");
    assert_eq!(pairs[0].b.as_str(), "FEAT-B");
}

#[test]
fn ac1_the_wire_shape_carries_pairs_for_the_board_badge() {
    // The board badge and the e2e mock pin the additive `derived.collisions`
    // contract: {"pairs":[{"a":..,"b":..,"files":[..]}],"unknown_files":[..]},
    // fields omitted when empty (absence = an empty radar for old clients).
    let state = state_with(vec![
        feature_at("FEAT-A", Status::InProgress, &["crates/app/src/main.rs"]),
        feature_at("FEAT-B", Status::InProgress, &["crates/app/src/main.rs"]),
        bug_at("CXC-B003", Status::InProgress, &[]),
    ]);
    let v = serde_json::to_value(collision_radar(&state)).expect("radar serializes");
    let pairs = v.get("pairs").and_then(|p| p.as_array()).expect("pairs");
    assert_eq!(pairs.len(), 1, "the colliding pair rides the wire");
    assert_eq!(pairs[0].get("a").and_then(|i| i.as_str()), Some("FEAT-A"));
    assert_eq!(pairs[0].get("b").and_then(|i| i.as_str()), Some("FEAT-B"));
    assert_eq!(
        pairs[0]
            .get("files")
            .and_then(|f| f.as_array())
            .map(std::vec::Vec::len),
        Some(1)
    );
    let unknown = v
        .get("unknown_files")
        .and_then(|u| u.as_array())
        .expect("the blind bug surfaces as unknown_files");
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].as_str(), Some("CXC-B003"));

    // And the omission rule: nothing to report → an EMPTY object, so the
    // snapshot inserts no `collisions` key at all.
    let empty =
        serde_json::to_value(collision_radar(&ProjectState::default())).expect("serializes");
    assert_eq!(empty, serde_json::json!({}), "absence = an empty radar");
}

/// The radar is advisory-only: it renders AFTER the claim succeeded and has
/// no write path. The DEV gate's selection must be identical whether or not
/// running slots collide — an advisory that bent selection would be a veto,
/// and the AC says the claim succeeds.
#[test]
fn guard_the_radar_is_advisory_only_and_never_touches_the_dev_gate() {
    let colliding = state_with(vec![
        feature_at("FEAT-A", Status::InProgress, &["crates/app/src/main.rs"]),
        feature_at("FEAT-B", Status::InProgress, &["crates/app/src/main.rs"]),
        feature_at("FEAT-C", Status::Ready, &["crates/app/src/main.rs"]),
    ]);
    let quiet = state_with(vec![
        feature_at("FEAT-A", Status::InProgress, &[]),
        feature_at("FEAT-B", Status::InProgress, &[]),
        feature_at("FEAT-C", Status::Ready, &["crates/app/src/main.rs"]),
    ]);
    assert_eq!(
        selection::next_ready_feature(&colliding),
        selection::next_ready_feature(&quiet),
        "the same Ready ticket is selectable whether or not the running slots \
         collide — the radar warns, the gate decides"
    );
    assert_eq!(
        selection::next_ready_feature(&colliding),
        Some(tid("FEAT-C")),
        "and specifically: FEAT-C is NOT blocked by the collision above it"
    );
    // The radar only advises tickets that are actually running: a Ready
    // ticket (not yet claimed) takes no advisory, and the colliding running
    // pair gets the WARNING — a prompt string, never an error or a refusal.
    assert_eq!(
        claim_warning(&colliding, &tid("FEAT-C")),
        "",
        "a Ready ticket is not a claim candidate — the advisory does not fire"
    );
    assert!(
        claim_warning(&colliding, &tid("FEAT-A")).contains("WARNING: slot collision"),
        "the running half of the pair does take the advisory"
    );
}

// --- AC2: two claims predicted to touch disjoint files produce no warning. ---

#[test]
fn ac2_disjoint_declared_files_stay_silent() {
    let state = state_with(vec![
        feature_at("FEAT-A", Status::InProgress, &["crates/app/src/main.rs"]),
        feature_at("FEAT-B", Status::InProgress, &["web/app.css"]),
    ]);
    let radar = collision_radar(&state);
    assert!(radar.pairs.is_empty(), "AC2: disjoint slots are no finding");
    assert!(radar.unknown_files.is_empty());
    assert_eq!(
        claim_warning(&state, &tid("FEAT-A")),
        "",
        "AC2: unrelated parallel work stays silent — '' when clean"
    );
}

// --- AC3: a ticket with no explicit file hints is visibly marked
//     low-confidence rather than silently skipped. ---

#[test]
fn ac3_a_ticket_without_file_hints_is_radar_blind_marked_low_confidence() {
    // CXC-B003 runs with a design that names no files: the radar cannot check
    // it, and it must SAY so — on the board (unknown_files) and in the brief.
    let state = state_with(vec![
        bug_at("CXC-B003", Status::InProgress, &[]),
        feature_at("FEAT-A", Status::InProgress, &["web/app.css"]),
    ]);
    let radar = collision_radar(&state);
    assert!(
        radar.unknown_files.contains(&tid("CXC-B003")),
        "AC3: the no-hint running ticket surfaces as radar-blind, not skipped"
    );
    assert!(
        radar.pairs.is_empty(),
        "AC3: blindness proves no overlap — no fake pair"
    );
    let warning = claim_warning(&state, &tid("CXC-B003"));
    assert!(
        warning.contains("low confidence") && warning.contains("declares no files"),
        "AC3: the brief block visibly marks the prediction low-confidence: {warning}"
    );
}

// --- AC4: with no usable repo map / design surface the radar degrades to no
//     warning and never fails or blocks a claim. ---

#[test]
fn ac4_a_state_without_usable_designs_degrades_and_never_fails() {
    // The pure derivation reads only what is IN the state (declared files) —
    // there is no repo map to load and nothing to fail: a state whose running
    // tickets carry no file hints degrades to unknown_files + the
    // low-confidence note, and every entry point returns.
    let state = state_with(vec![
        bug_at("CXC-B003", Status::InProgress, &[]),
        feature_at("FEAT-A", Status::InProgress, &["   "]),
    ]);
    let radar = collision_radar(&state);
    assert!(radar.pairs.is_empty(), "AC4: nothing usable → no pair");
    assert_eq!(
        radar.unknown_files,
        vec![tid("CXC-B003"), tid("FEAT-A")],
        "AC4: both running tickets are radar-blind, sorted, not silently OK"
    );
    for id in ["CXC-B003", "FEAT-A", "FEAT-ZZZ"] {
        let _ = claim_warning(&state, &tid(id));
    }
    // And the empty project state degrades the same way — no panic, no pair.
    let radar = collision_radar(&ProjectState::default());
    assert!(radar.pairs.is_empty() && radar.unknown_files.is_empty());
}

// --- AC5: warnings never fire across projects. ---

#[test]
fn ac5_the_radar_only_sees_the_one_project_state_it_is_handed() {
    // Two projects run the same file in parallel slots. Each state's radar
    // knows ONLY its own tickets: the derivation takes one ProjectState and
    // has no other project in scope, so a cross-project warning cannot be
    // produced — pinned by construction.
    let project_one = state_with(vec![feature_at(
        "FEAT-A",
        Status::InProgress,
        &["crates/app/src/main.rs"],
    )]);
    let project_two = state_with(vec![feature_at(
        "FEAT-A2",
        Status::InProgress,
        &["crates/app/src/main.rs"],
    )]);
    let radar_one = collision_radar(&project_one);
    assert!(
        radar_one.pairs.is_empty(),
        "AC5: a lone running ticket per project is no collision in EITHER project"
    );
    let known: BTreeSet<String> = project_one
        .tickets
        .iter()
        .map(|t| t.id().to_string())
        .collect();
    for pair in &radar_one.pairs {
        assert!(
            known.contains(pair.a.as_str()) && known.contains(pair.b.as_str()),
            "AC5: every reported id belongs to the handed state — never the other project"
        );
    }
    assert!(
        collision_radar(&project_two).pairs.is_empty(),
        "AC5: the second project's same-file slot stays in its own project"
    );
}
