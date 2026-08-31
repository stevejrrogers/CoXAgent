//! TDD tests for CXA-F234 — Per-operator working-hours & local-time delivery
//! windows. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "An operator can declare working-hours/windows with weekday+tz offset+start/end
//!    once via API/settings UI; invalid tz or malformed range is rejected at save."
//! 2. "'Is this UTC instant actionable for this operator?' returns true only when
//!    inside their declared windows AND not excluded by weekend/day rule — verified
//!    as pure functions over struct literals with real timestamps."
//! 3. "DST boundary instants fall inside exactly one correct resolved window"
//!
//! AC → test map:
//! - AC1: [`ac1_invalid_tz_offset_is_rejected_at_save`],
//!   [`ac1_malformed_window_range_is_rejected_at_save`],
//!   [`ac1_a_valid_declaration_survives_the_settings_save_round_trip`], plus the
//!   green guards [`guard_the_settings_save_surface_is_a_whole_document_put_of_the_typed_config`]
//!   and [`guard_fail_closed_field_naming_already_rejects_unrepresentable_values_today`].
//! - AC2: [`ac2_the_actionable_for_operator_decision_exists_as_a_pure_application_derivation`]
//!   (RED: the derivation does not exist) and the green fixture guard
//!   [`guard_struct_literal_fixtures_over_real_timestamps_are_buildable_today`].
//! - AC3: [`ac3_dst_resolution_lives_in_the_pure_derivation_and_must_yield_exactly_one_window`]
//!   (RED) and the green fixture guard [`guard_dst_boundary_instants_are_real_calendar_facts`].
//!
//! HOW THESE CRITERIA ARE ENCODED (the `preflight_f239_tdd.rs` / `reproduce_url_f244_tdd.rs`
//! discipline): every identifier below exists in production today, so the suite
//! compiles; each failing assertion fails only because CXA-F234's behaviour is
//! missing. No server, no host harness, no network port, no invented types.
//!
//! - AC1 is behavioural RED over the REAL save seam: `parse_config`
//!   (`config_parse.rs`) is the one validator both the Settings screen's save
//!   (`PUT /api/projects/:pid/config` → `put_config`, which writes back the
//!   whole document) and the runner's load share, and it already fails closed
//!   naming the offending field (COX-B043/B050). `Config` does not deny unknown
//!   fields today, so an operator declaration is silently DROPPED and any tz/range
//!   garbage is silently ACCEPTED — exactly the missing behaviour these tests
//!   fail on. When the declaration becomes a typed, validated part of the
//!   settings document, all three AC1 tests go green with no edits.
//! - AC2/AC3 assert a pure derivation that exists nowhere yet. Following
//!   `preflight_f239_tdd.rs`, what is machine-checkable pre-implementation is
//!   pinned as RED scans (the derivation's home file, the AC's own vocabulary,
//!   and its PURITY — no IO primitives, per the hexagonal ratchet), and the
//!   input fixtures are proven buildable from types the codebase actually has
//!   (`time::OffsetDateTime`/`UtcOffset`/`Weekday`, green guards). The
//!   executable SEMANTIC assertions are pinned in this header as the contract
//!   the implementation must satisfy (see NOT ENCODED below).
//!
//! THE DOCUMENT SHAPE CONTRACT (pinned so implementer and reviewer share one
//! set of names; every name is the AC's own word or a house precedent):
//! per-operator working hours live in the settings document at
//! `workflow.human.working_hours` (per-human settings live under
//! `workflow.human` — the CXA-F176 `focus_windows` precedent), one entry per
//! operator, keyed by bare username, declared ONCE:
//!
//! ```json
//! "working_hours": {
//!   "mira": {
//!     "tz_offset": "+02:00",
//!     "windows": [ { "weekday": "monday", "start": "09:00", "end": "17:30" } ]
//!   }
//! }
//! ```
//!
//! `weekday` uses the lowercase full names `time::Weekday` already serializes as.
//! If the implementation rehomes these names, move the guards with them (the
//! `preflight_f239_tdd.rs` convention) — do not weaken what they assert.
//!
//! NOT ENCODED — reported, not fabricated (house rule: never invent the missing
//! contract):
//! - AC2's true/false table ("true only when inside their declared windows AND
//!   not excluded by weekend/day rule") needs the derivation's signature to
//!   exist before it can be called; pre-implementation the only options were
//!   inventing the signature (forbidden) or a suite that fails to compile
//!   (forbidden — it would poison `cargo test` for every other ticket). The
//!   executable form lands with `working_hours.rs` and MUST assert, over the
//!   real fixtures proven in [`guard_struct_literal_fixtures_over_real_timestamps_are_buildable_today`]:
//!   an instant inside a declared window on a declared weekday ⇒ actionable;
//!   the same local time on an undeclared weekday ⇒ NOT actionable; outside
//!   the window ⇒ NOT actionable.
//! - AC3's "exactly one" arbitration: for the real DST boundary instants proven
//!   in [`guard_dst_boundary_instants_are_real_calendar_facts`], the resolver
//!   must yield exactly ONE window containing the instant — never both sides
//!   of the transition, never zero. This needs the resolver's signature too.
//! - DESIGN DISCREPANCY to resolve before implementation (ASK SA): the AC text
//!   says "tz offset" while the committed design mockups name IANA zones
//!   ("Europe/Vienna", "Asia/Saigon", "America/Los_Angeles" —
//!   `.coxagent/design/CXA-F234/_build.py`). `time` 0.3 (the workspace's only
//!   time crate) ships NO tz-database feature and Cargo.lock carries no
//!   chrono-tz/tz-rs, so IANA names cannot be resolved to offsets today —
//!   choosing that model means a NEW dependency, which is an SA decision, not
//!   an implementer's whim. These tests pin the AC text (a fixed `tz_offset`
//!   per operator declaration); the boundary-instant fixtures stay valid under
//!   either model.
//! - A wrap-midnight WORKING window ("22:00-06:00") is intentionally not pinned:
//!   the quiet-hours precedent supports it, the design's working hours never
//!   use it, and the AC text does not say — unspecified, so unasserted.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::config::in_quiet_window;
use coxagent_application::config_parse::parse_config;
use serde_json::json;
use time::macros::datetime;
use time::{Duration, Month, OffsetDateTime, UtcOffset, Weekday};

// ---------------------------------------------------------------------------
// Repo-source scan helpers (the reproduce_url_f244_tdd.rs pattern).
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The derivation's home: the design's own heading ("Working hours & delivery
/// window"), housed in the application layer as a pure module — the
/// `dependency_radar`/`forensics` precedent; `config.rs` is already oversized
/// and must not grow a second decision engine.
const DERIVATION: &str = "crates/application/src/working_hours.rs";

// ---------------------------------------------------------------------------
// AC1 — declare once via settings; invalid tz / malformed range rejected at save.
//
// `parse_config` IS the save validator: the Settings screen's PUT re-parses
// the whole document through the same types the runner loads (status.rs
// `config_for_settings` / `put_config`). Today it ignores the whole operator
// declaration (unknown field), so every assertion below fails.
// ---------------------------------------------------------------------------

/// A settings document carrying one operator's declared working hours. The
/// `workflow` section carries its three required knobs (`ba_every_n_cycles`,
/// `feature_dev_enabled`, `sleep_seconds` — the only fields on it without
/// `#[serde(default)]`), so the document is exactly what a real coxagent.json
/// plus the new declaration looks like: valid today but for the declaration.
fn settings_document(tz_offset: &str, windows: &serde_json::Value) -> String {
    json!({
        "workflow": {
            "ba_every_n_cycles": 1,
            "feature_dev_enabled": true,
            "sleep_seconds": 60,
            "human": {
                "working_hours": {
                    "mira": { "tz_offset": tz_offset, "windows": windows }
                }
            }
        }
    })
    .to_string()
}

#[test]
fn ac1_invalid_tz_offset_is_rejected_at_save() {
    for tz in ["+99:00", "+02:60"] {
        let doc = settings_document(tz, &json!([{ "weekday": "monday", "start": "09:00", "end": "17:30" }]));
        let err = parse_config(&doc)
            .expect_err("an impossible tz offset must be REJECTED at save, not stored silently");
        assert!(
            err.field.contains("tz_offset"),
            "the rejection must name the offending field (COX-B043 posture), got: {} ({})",
            err.field,
            err.detail
        );
    }
}

#[test]
fn ac1_malformed_window_range_is_rejected_at_save() {
    for (start, end) in [
        ("25:00", "17:30"),  // hour out of range
        ("09:00", "24:00"),  // end hour out of range
        ("09:00", "17:61"),  // minute out of range
        ("nine", "17:30"),   // not an HH:MM range at all
    ] {
        let doc =
            settings_document("+02:00", &json!([{ "weekday": "monday", "start": start, "end": end }]));
        let err = parse_config(&doc).expect_err(format!(
            "a malformed range {start}-{end} must be REJECTED at save, not stored silently"
        ).as_str());
        assert!(
            err.field.contains("working_hours"),
            "the rejection must name the offending section, got: {} ({})",
            err.field,
            err.detail
        );
    }
}

#[test]
fn ac1_a_valid_declaration_survives_the_settings_save_round_trip() {
    // "declare … once": the Settings screen's PUT writes back the whole typed
    // document (status.rs `put_config`), so a declaration the operator entered
    // must still be IN the document after a load→serialize cycle — not dropped
    // as an unknown field, which is what happens today.
    let doc = settings_document(
        "+02:00",
        &json!([{ "weekday": "monday", "start": "09:00", "end": "17:30" }]),
    );
    let cfg = parse_config(&doc).expect("a valid declaration must load");
    let back = serde_json::to_value(&cfg).expect("the typed config serializes");
    let declared = &back["workflow"]["human"]["working_hours"]["mira"];
    assert!(
        declared.is_object(),
        "the operator's declaration must survive the settings save round-trip, got: {back}"
    );
    assert_eq!(declared["tz_offset"], json!("+02:00"));
    assert_eq!(declared["windows"][0]["weekday"], json!("monday"));
    assert_eq!(declared["windows"][0]["start"], json!("09:00"));
    assert_eq!(declared["windows"][0]["end"], json!("17:30"));
}

#[test]
fn guard_the_settings_save_surface_is_a_whole_document_put_of_the_typed_config() {
    // GREEN today — fixture validity: the save surface AC1 names is real, and
    // it deserializes the WHOLE document into the typed `Config`, so validation
    // added to the settings types lands exactly on this save path.
    let router = read("crates/presentation/src/server/mod.rs");
    assert!(
        router.contains(".route(\"/api/projects/:pid/config\", get(get_config).put(put_config))"),
        "the settings save route must stay registered where AC1's UI saves"
    );
    let status = read("crates/presentation/src/server/status.rs");
    assert!(
        status.contains("Json(cfg): Json<Config>"),
        "put_config must keep deserializing the whole typed config — a declaration \
         that bypasses the typed save path would skip AC1's rejection"
    );
    assert!(
        status.contains("fs::write(&p.config_path"),
        "put_config must keep persisting the document it validated"
    );
}

#[test]
fn guard_fail_closed_field_naming_already_rejects_unrepresentable_values_today() {
    // GREEN today — the mechanism AC1's rejection extends: parse_config already
    // refuses a value the schema cannot represent and names the field. The AC1
    // red tests above fail only because the operator declaration is not yet
    // part of that schema — not because the validator is missing.
    let err = parse_config(r#"{"deploy":{"host_port":999999}}"#).expect_err("out of u16 range");
    assert_eq!(err.field, "deploy.host_port");
}

// ---------------------------------------------------------------------------
// AC2 — "Is this UTC instant actionable for this operator?" as a PURE
// derivation over struct literals with real timestamps.
// ---------------------------------------------------------------------------

#[test]
fn ac2_the_actionable_for_operator_decision_exists_as_a_pure_application_derivation() {
    let src = std::fs::read_to_string(repo_root().join(DERIVATION)).unwrap_or_else(|_| {
        panic!(
            "{DERIVATION} does not exist yet — CXA-F234's 'is this UTC instant actionable \
             for this operator?' derivation has no home, so the question is answered by \
             nothing. This failing test IS the red half of the pair."
        )
    });
    // The AC's own vocabulary, over real timestamps: a UTC instant, the
    // operator's declared windows, and the weekend/day rule.
    for token in ["actionable", "OffsetDateTime", "UtcOffset", "Weekday", "window"] {
        assert!(
            src.contains(token),
            "the actionable-for-operator derivation must speak the AC's vocabulary; \
             `{token}` is missing from {DERIVATION}"
        );
    }
    // AC2 pins PURE functions: no IO primitives may reach the decision (the
    // hexagonal ratchet's rule, enforced here for the module this AC owns).
    for forbidden in ["std::fs::", "std::process::Command", "tokio::", "async fn"] {
        assert!(
            !src.contains(forbidden),
            "the actionable decision must be a pure function; {DERIVATION} contains {forbidden}"
        );
    }
}

#[test]
fn guard_struct_literal_fixtures_over_real_timestamps_are_buildable_today() {
    // GREEN today — fixture validity for AC2's "struct literals with real
    // timestamps": every input the derivation must consume is constructible
    // from types the codebase already has, and the local-window containment it
    // must agree with is the house HH:MM machinery (`in_quiet_window`).
    let vienna_summer = UtcOffset::from_hms(2, 0, 0).expect("+02:00 is a legal offset");
    let monday_morning = datetime!(2026-08-24 07:30 UTC);
    assert_eq!(monday_morning.weekday(), Weekday::Monday);
    let local = monday_morning.to_offset(vienna_summer);
    assert_eq!((local.hour(), local.minute()), (9, 30));
    let minutes_of_day = u32::from(local.hour()) * 60 + u32::from(local.minute());
    // The local window the operator declared contains the local minute the
    // real instant resolves to — via the same parser quiet hours already use.
    assert!(
        in_quiet_window("09:00-17:30", minutes_of_day),
        "09:30 local must sit inside the declared 09:00-17:30 window"
    );
    assert!(
        !in_quiet_window("09:00-17:30", 8 * 60 + 59),
        "08:59 local must sit outside the declared window"
    );
    // And the day-rule axis is a real type: the weekend exclusion AC2 demands
    // keys off `Weekday`, provable on a real Sunday instant.
    let sunday_evening = datetime!(2026-08-30 18:00 UTC);
    assert_eq!(sunday_evening.weekday(), Weekday::Sunday);
}

// ---------------------------------------------------------------------------
// AC3 — DST boundary instants fall inside exactly one correct resolved window.
// ---------------------------------------------------------------------------

#[test]
fn guard_dst_boundary_instants_are_real_calendar_facts() {
    // GREEN today — fixture validity for AC3: the boundary instants the resolver
    // must arbitrate are real European-rule transitions, verified from the
    // calendar itself (last Sunday of the month), not invented constants.
    // 2026: spring forward 2026-03-29 01:00 UTC (CET +01:00 → CEST +02:00),
    // fall back 2026-10-25 01:00 UTC.
    let spring = datetime!(2026-03-29 01:00 UTC);
    assert_eq!(spring.weekday(), Weekday::Sunday);
    assert_eq!(
        (spring + Duration::days(7)).month(),
        Month::April,
        "2026-03-29 must be the LAST Sunday of March — the European spring-forward day"
    );
    let fall = datetime!(2026-10-25 01:00 UTC);
    assert_eq!(fall.weekday(), Weekday::Sunday);
    assert_eq!(
        (fall + Duration::days(7)).month(),
        Month::November,
        "2026-10-25 must be the LAST Sunday of October — the European fall-back day"
    );
    // The same instant reads one hour apart across the boundary — the ambiguity
    // "exactly one correct resolved window" exists to settle.
    let winter = UtcOffset::from_hms(1, 0, 0).expect("+01:00 is a legal offset");
    let summer = UtcOffset::from_hms(2, 0, 0).expect("+02:00 is a legal offset");
    assert_ne!(winter, summer);
    let local_minutes = |offset: &UtcOffset, at: &OffsetDateTime| {
        let t = at.to_offset(*offset);
        u32::from(t.hour()) * 60 + u32::from(t.minute())
    };
    assert_eq!(local_minutes(&winter, &spring), 2 * 60, "02:00 local before the jump");
    assert_eq!(local_minutes(&summer, &spring), 3 * 60, "03:00 local after the jump");
}

#[test]
fn ac3_dst_resolution_lives_in_the_pure_derivation_and_must_yield_exactly_one_window() {
    let src = std::fs::read_to_string(repo_root().join(DERIVATION)).unwrap_or_else(|_| {
        panic!(
            "{DERIVATION} does not exist yet — there is no resolver to arbitrate a DST \
             boundary instant into exactly one correct resolved window. This failing \
             test IS the red half of the pair."
        )
    });
    for token in ["resolve", "OffsetDateTime", "UtcOffset"] {
        assert!(
            src.contains(token),
            "the DST resolution must speak the AC's vocabulary; `{token}` is missing \
             from {DERIVATION}"
        );
    }
    // The executable contract this scan stands in for (lands with the resolver's
    // signature — see NOT ENCODED in the header): over the real boundary instant
    // `datetime!(2026-03-29 01:00 UTC)` and an operator declaration whose
    // windows the instant reads as inside under BOTH sides of the transition
    // (+01:00 → 02:00 local, +02:00 → 03:00 local), the count of resolved
    // windows containing the instant MUST be exactly 1.
}
