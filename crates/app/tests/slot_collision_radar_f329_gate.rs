//! CXA-F329 acceptance gate — "Slot collision radar: warn at claim time when
//! two claimed tickets will touch the same files".
//!
//! Same discipline as `fleet_river_f233_gate.rs`: the wiring invariants are
//! pinned as scans of the source-of-truth files (a pure-derivation feature
//! has no route to register — its delivery is two thin seams), and the data
//! invariant runs over the real production types (the serialized wire shape
//! the board badge and the e2e mock depend on). PURE: no fake HTTP server, no
//! host harness, no network port.
//!
//! AC → test map:
//! - AC1 (board + brief show the warning, claim never blocked):
//!   [`ac1_the_brief_seam_embeds_the_claim_time_warning`],
//!   [`ac1_the_snapshot_seam_rides_derived_collisions`],
//!   [`ac1_the_board_badge_reads_derived_collisions`],
//!   [`ac1_the_warning_is_a_string_the_claim_flow_never_reads`]
//! - AC3 (low-confidence marking visible):
//!   [`ac1_the_board_badge_reads_derived_collisions`] (UNMAPPED marker),
//!   [`the_wire_shape_omits_empty_fields_absence_is_an_empty_radar`]
//! - AC5 (never across projects): the derivation signature takes exactly one
//!   `ProjectState` — pinned in the TDD suite; this gate pins that no other
//!   project source is reachable from the radar module (it imports no store,
//!   no port, no registry).
//!
//! The pure-layer ACs (overlap detection, disjoint silence, blind handling,
//! determinism) are executable in the radar module's own tests and the
//! `slot_collision_radar_f329_tdd` suite; this gate pins the DELIVERY seams.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::slot_collision_radar::{claim_warning, collision_radar, CollisionPair};
use coxagent_application::state::ProjectState;
use coxagent_domain::{
    Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};

// --- repo-state scan helpers (the fleet_river_f233_gate.rs pattern) ----------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("source of truth {rel} must exist: {e}"))
}

// --- fixtures ----------------------------------------------------------------

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

fn running(id: &str, files: &[&str]) -> Ticket {
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
    t.transition_to(Role::Sa, Status::Ready).expect("ready");
    t.transition_to(Role::DevFeature, Status::InProgress)
        .expect("in progress");
    t
}

// --- AC1: the two delivery seams exist and are wired -------------------------

/// The claiming agent's brief embeds the claim-time advisory: the run_dev
/// briefing calls the radar's `claim_warning` with the post-claim state and
/// the claimed ticket id.
#[test]
fn ac1_the_brief_seam_embeds_the_claim_time_warning() {
    let briefing = read("crates/application/src/use_cases/run_dev/briefing.rs");
    assert!(
        briefing.contains("slot_collision_radar::claim_warning(state, id)"),
        "the brief must render the claim-time advisory for the claimed ticket"
    );
    assert!(
        briefing.contains("{collision_warning}"),
        "the rendered block must actually flow into the task prompt format"
    );
}

/// The 1 Hz snapshot / GET state seam rides the additive `derived.collisions`
/// key: lite_state_value computes the whole-state radar and merges it into
/// the same `derived` object the F237 dependency radar already owns.
#[test]
fn ac1_the_snapshot_seam_rides_derived_collisions() {
    let status = read("crates/presentation/src/server/status.rs");
    assert!(
        status.contains("slot_collision_radar::collision_radar"),
        "the snapshot must derive the collision radar"
    );
    assert!(
        status.contains("\"collisions\""),
        "the additive key inside `derived` is named collisions (the design's API contract)"
    );
}

/// The board badge reads exactly the wire shape the snapshot serves — the
/// COLLISION marker for paired tickets and the UNMAPPED marker (low
/// confidence) for radar-blind ones — and home.js is actually loaded by the
/// page (the CXA-F233 lesson: a served-but-never-loaded script ships a dead
/// view).
#[test]
fn ac1_the_board_badge_reads_derived_collisions() {
    let home = read("crates/presentation/src/web/js/home.js");
    assert!(
        home.contains("function collisionBadge(s,t)"),
        "the board badge lives beside the F237 blockedBadge in home.js"
    );
    assert!(
        home.contains("collisions"),
        "the badge reads the derived.collisions payload"
    );
    assert!(
        home.contains("UNMAPPED"),
        "AC3: radar-blind running tickets are visibly marked low-confidence"
    );
    let index = read("crates/presentation/src/web/index.html");
    assert!(
        index.contains("<script src=\"/assets/js/home.js\"></script>"),
        "the badge's script must be LOADED, not only served (CXA-F233's dead-menu lesson)"
    );
}

// --- the wire contract over the REAL production types ------------------------

/// The serialized radar matches the design's API contract exactly:
/// `{"pairs":[{"a":..,"b":..,"files":[..]}],"unknown_files":[..]}` — the shape
/// the board badge (home.js) and the e2e spec mock are written against.
#[test]
fn the_wire_shape_matches_the_design_contract() {
    let state = ProjectState {
        tickets: vec![
            running("FEAT-A", &["crates/app/src/main.rs"]),
            running("FEAT-B", &["crates/app/src/main.rs"]),
        ],
        ..ProjectState::default()
    };
    let v = serde_json::to_value(collision_radar(&state)).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "pairs": [{"a": "FEAT-A", "b": "FEAT-B", "files": ["crates/app/src/main.rs"]}]
        }),
        "exact field names, sorted ids, files present — nothing invented"
    );
}

/// Fields the project has nothing to report are OMITTED: absence of the
/// `collisions` key IS an empty radar for every existing client (the F237
/// omission rule, extended to F329).
#[test]
fn the_wire_shape_omits_empty_fields_absence_is_an_empty_radar() {
    let v = serde_json::to_value(collision_radar(&ProjectState::default())).unwrap();
    assert_eq!(v, serde_json::json!({}), "no findings → no fields at all");
    // A blind-only state: pairs omitted, unknown_files present.
    let state = ProjectState {
        tickets: vec![running("FEAT-A", &[])],
        ..ProjectState::default()
    };
    let v = serde_json::to_value(collision_radar(&state)).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"unknown_files": ["FEAT-A"]}),
        "radar-blind is surfaced; the empty half is omitted"
    );
}

/// The radar is a pure advisory: `claim_warning` returns a prompt STRING —
/// there is no error type, no Result, no refusal it could produce, and the
/// DEV gate's selection is untouched by the radar's findings (advisory-only,
/// the claim has already succeeded when the warning renders).
#[test]
fn ac1_the_warning_is_a_string_the_claim_flow_never_reads() {
    let state = ProjectState {
        tickets: vec![
            running("FEAT-A", &["crates/app/src/main.rs"]),
            running("FEAT-B", &["crates/app/src/main.rs"]),
        ],
        ..ProjectState::default()
    };
    let pairs: Vec<CollisionPair> = collision_radar(&state).pairs;
    assert_eq!(pairs.len(), 1);
    // The warning is a plain String for the brief: '' when clean, prose when
    // colliding — nothing for the claim flow to branch on.
    assert!(claim_warning(&state, &tid("FEAT-A")).starts_with("\nWARNING: slot collision"));
    assert_eq!(claim_warning(&state, &tid("FEAT-ZZZ")), "");
}

/// The radar module is pure: it imports no store, no port, no registry — so
/// a cross-project comparison (AC5) has no input path by construction, and
/// the hexagonal ratchet has nothing to flag.
#[test]
fn ac5_the_radar_module_reaches_no_store_or_port() {
    let src = read("crates/application/src/slot_collision_radar.rs");
    for banned in [
        "StateStorePort",
        "ports::outbound",
        "std::fs",
        "std::process",
        "tokio",
        "reqwest",
    ] {
        assert!(
            !src.replace("#[cfg(test)]", "SPLIT")
                .split("SPLIT")
                .next()
                .is_some_and(|prod| prod.contains(banned)),
            "the radar's production half must not reach for {banned} — it is a pure derivation"
        );
    }
}
