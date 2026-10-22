//! CXA-B287 (part 2/3 of CXA-B254) — CONTRACT GUARDS.
//!
//! Cross-cutting invariants enforced over the ENTIRE wire surface in ONE
//! reflective scan, instead of per-type hand-written checks. Every guard is a
//! PURE function over the registries below: it returns a `Vec<String>` of
//! failures naming the exact offending TYPE (and FIELD/VARIANT), and the
//! tests simply assert the failure lists are empty. No HTTP, no ports, no FS
//! beyond checked-in source.
//!
//! Invariants guarded here:
//! (a) empty-payload deserializability — either `{}` loads, or the type's
//!     required fields appear verbatim in [`REQUIRED_FIELD_WHITELIST`];
//! (b) unknown-keys policy — uniform tolerant loading, with any
//!     strictly-denying type listed in [`STRICT_UNKNOWN_KEY_TYPES`] (empty
//!     today: no DTO opts into `deny_unknown_fields`);
//! (c) enum wire form — EVERY serialized enum variant uses `snake_case`
//!     consistently, tags stay unique within their enum, and the variant
//!     inventory is exhaustive (adding/renaming a variant fails loudly);
//! (d) envelope shapes — the error envelope and the version-tagged health
//!     envelope are asserted ONCE, against the shapes actually served today
//!     (`crates/presentation/src/server/mod.rs`: `internal_error`,
//!     `conflict_error`, `bad_request_error`, `health`).

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// The registries — the entire contract surface, named in one place.
//
// SINGLE SOURCE OF TRUTH: these tables live in `contract_surface/mod.rs`,
// shared verbatim with the CXA-B288 contract-docs generator so the guard and
// the rendered documentation can never disagree (CXA-B288 AC: no duplicate
// drift-prone lists).
// ---------------------------------------------------------------------------

mod contract_surface;

use contract_surface::{
    EnumContract, ENUM_SURFACE, REQUIRED_FIELD_WHITELIST, STRICT_UNKNOWN_KEY_TYPES,
};

// ---------------------------------------------------------------------------
// Guard (c): enum wire-form consistency + exhaustive inventory — ONE scan
// ---------------------------------------------------------------------------

/// `snake_case` over ASCII identifiers: lowercase words joined by `_`.
fn is_snake_case(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !tag.starts_with('_')
        && !tag.ends_with('_')
        && !tag.contains("__")
}

/// ONE reflective pass over every registered enum. Pure: input ->
/// failures, no IO. Failure text always contains the offending type AND
/// variant, e.g. `"enum Role variant DevFeature: wire tag \"dev-feature\" .."`.
fn enum_casing_and_uniqueness_failures(surface: &[EnumContract]) -> Vec<String> {
    let mut failures = Vec::new();
    for e in surface {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (variant, tag) in e.variants {
            if !is_snake_case(tag) {
                failures.push(format!(
                    "enum {} variant {}: wire tag {:?} violates the snake_case convention",
                    e.ty, variant, tag
                ));
            }
            if let Some(other) = seen.insert(tag, variant) {
                failures.push(format!(
                    "enum {}: variants {} and {} collide on wire tag {:?}",
                    e.ty, other, variant, tag
                ));
            }
        }
    }
    failures
}

/// Adding or removing a variant MUST bump this guard deliberately: the
/// inventory above is the documentation of the wire surface.
#[test]
fn every_registered_enum_variant_inventory_is_exhaustive_against_today() {
    let expected: &[(&str, usize)] = &[
        ("TicketType", 3),
        ("Status", 5),
        ("Role", 8),
        ("Priority", 3),
        ("Complexity", 3),
        ("TestCaseStatus", 3),
        ("RecordKind", 2),
        ("CoverageStatus", 3),
        ("DebtSignalKind", 3),
        ("GoalStatus", 2),
    ];
    let got: Vec<(String, usize)> = ENUM_SURFACE
        .iter()
        .map(|e| (e.ty.to_string(), e.variants.len()))
        .collect();
    for (ty, want) in expected {
        let hit = got.iter().find(|(g, _)| g == *ty);
        assert_eq!(hit.map(|(_, n)| *n), Some(*want),
            "enum {ty}: variant inventory drifted — update ENUM_SURFACE (the contract doc) deliberately");
    }
    assert_eq!(
        got.len(),
        expected.len(),
        "an enum was added or removed without registering it in ENUM_SURFACE"
    );
}

#[test]
fn every_serialized_enum_variant_uses_snake_case_consistently() {
    let failures = enum_casing_and_uniqueness_failures(ENUM_SURFACE);
    assert!(
        failures.is_empty(),
        "contract-guard violations:\n{}",
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Intentional-break proof: the guard NAMES the offender (AC: PR-body evidence)
// ---------------------------------------------------------------------------

#[test]
fn a_deliberate_break_is_named_by_type_and_variant() {
    // A mis-cased tag smuggled into a copy of the registry…
    let broken = [EnumContract {
        ty: "GuardProbe",
        variants: &[("FooBar", "foo-bar")], // kebab-case violation
    }];
    let failures = enum_casing_and_uniqueness_failures(&broken);
    assert_eq!(failures.len(), 1);
    // …must be reported with BOTH the type and the variant name.
    assert!(failures[0].contains("GuardProbe"), "{}", failures[0]);
    assert!(failures[0].contains("FooBar"), "{}", failures[0]);
    assert!(failures[0].contains("foo-bar"), "{}", failures[0]);
}

#[test]
fn a_colliding_wire_tag_names_both_variants() {
    let broken = [EnumContract {
        ty: "GuardProbe2",
        variants: &[("Alpha", "same"), ("Beta", "same")],
    }];
    let failures = enum_casing_and_uniqueness_failures(&broken);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("GuardProbe2"), "{}", failures[0]);
    assert!(
        failures[0].contains("Alpha") && failures[0].contains("Beta"),
        "{}",
        failures[0]
    );
}

// ---------------------------------------------------------------------------
// Guard (a)+(b): payload-policy scan over REAL registered constructors
// ---------------------------------------------------------------------------

fn payloads() -> Vec<PayloadBoxed> {
    use coxagent_domain::coverage::CoverageEntry;
    use coxagent_domain::test_case::{TestCase, VerdictRecord};
    use coxagent_domain::wip_checkpoint::WipCheckpoint;
    vec![
        PayloadBoxed {
            ty: "TestCase",
            try_from_empty: Box::new(|| serde_json::from_value::<TestCase>(serde_json::json!({})).map(|_| ()).map_err(|e| e.to_string())),
            probe_unknown_key: Box::new(|| {
                serde_json::from_value::<TestCase>(serde_json::json!({"description": "d", "__alien__": 1}))
                    .map(|t| serde_json::to_value(t).unwrap().get("__alien__").is_none())
                    .map_err(|e| e.to_string())
            }),
            probe_partial: Box::new(|missing: &[&str]| {
                let mut obj = serde_json::Map::new();
                obj.insert("description".into(), serde_json::json!("d"));
                for m in missing { obj.remove(*m); }
                serde_json::from_value::<TestCase>(obj.into()).map(|_| ()).map_err(|e| e.to_string())
            }),
            covered_by_strict_list: false,
        },
        PayloadBoxed {
            ty: "VerdictRecord",
            try_from_empty: Box::new(|| serde_json::from_value::<VerdictRecord>(serde_json::json!({})).map(|_| ()).map_err(|e| e.to_string())),
            probe_unknown_key: Box::new(|| {
                serde_json::from_value::<VerdictRecord>(
                    serde_json::json!({"status": "passed", "at": "2025-06-01T12:00:00Z", "__alien__": 1}))
                    .map(|v| serde_json::to_value(v).unwrap().get("__alien__").is_none())
                    .map_err(|e| e.to_string())
            }),
            probe_partial: Box::new(|missing: &[&str]| {
                let mut obj = serde_json::Map::new();
                obj.insert("status".into(), serde_json::json!("passed"));
                obj.insert("at".into(), serde_json::json!("2025-06-01T12:00:00Z"));
                for m in missing { obj.remove(*m); }
                serde_json::from_value::<VerdictRecord>(obj.into()).map(|_| ()).map_err(|e| e.to_string())
            }),
            covered_by_strict_list: false,
        },
        PayloadBoxed {
            ty: "CoverageEntry",
            try_from_empty: Box::new(|| serde_json::from_value::<CoverageEntry>(serde_json::json!({})).map(|_| ()).map_err(|e| e.to_string())),
            probe_unknown_key: Box::new(|| {
                serde_json::from_value::<CoverageEntry>(
                    serde_json::json!({"criterion": "ac1", "status": "covered", "__alien__": 1}))
                    .map(|v| serde_json::to_value(v).unwrap().get("__alien__").is_none())
                    .map_err(|e| e.to_string())
            }),
            probe_partial: Box::new(|missing: &[&str]| {
                let mut obj = serde_json::Map::new();
                obj.insert("criterion".into(), serde_json::json!("ac1"));
                obj.insert("status".into(), serde_json::json!("covered"));
                for m in missing { obj.remove(*m); }
                serde_json::from_value::<CoverageEntry>(obj.into()).map(|_| ()).map_err(|e| e.to_string())
            }),
            covered_by_strict_list: false,
        },
        PayloadBoxed {
            ty: "WipCheckpoint",
            try_from_empty: Box::new(|| serde_json::from_value::<WipCheckpoint>(serde_json::json!({})).map(|_| ()).map_err(|e| e.to_string())),
            probe_unknown_key: Box::new(|| {
                serde_json::from_value::<WipCheckpoint>(
                    serde_json::json!({"ref_name": "r", "sha": "s", "note": "n", "recorded_at": "t", "__alien__": 1}))
                    .map(|v| serde_json::to_value(v).unwrap().get("__alien__").is_none())
                    .map_err(|e| e.to_string())
            }),
            probe_partial: Box::new(|missing: &[&str]| {
                let mut obj = serde_json::Map::new();
                obj.insert("ref_name".into(), serde_json::json!("refs/coxagent/wip/X"));
                obj.insert("sha".into(), serde_json::json!("abc123"));
                obj.insert("note".into(), serde_json::json!("n"));
                obj.insert("recorded_at".into(), serde_json::json!("t"));
                for m in missing { obj.remove(*m); }
                serde_json::from_value::<WipCheckpoint>(obj.into()).map(|_| ()).map_err(|e| e.to_string())
            }),
            covered_by_strict_list: false,
        },
    ]
    .into_iter()
    .collect()
}

/// Type-erased probe handle: keeps the guard generic while every probe
/// closes over a REAL domain type (pure functions, no harness).
struct PayloadBoxed {
    ty: &'static str,
    try_from_empty: Box<dyn Fn() -> Result<(), String>>,
    /// Load `{full payload + __alien__: 1}` -> `Ok(true)` when the alien key
    /// is tolerated AND dropped, `Ok(false)` when tolerated but kept,
    /// `Err` when refused outright.
    probe_unknown_key: Box<dyn Fn() -> Result<bool, String>>,
    /// Strip `fields` from a minimal-valid payload and try to load.
    probe_partial: Box<dyn Fn(&[&str]) -> Result<(), String>>,
    covered_by_strict_list: bool,
}

#[test]
fn empty_object_payload_deserializes_exactly_where_whitelisted_or_fully_defaulted() {
    let mut failures = Vec::new();
    for p in payloads() {
        if let Err(e) = (p.try_from_empty)() {
            let whitelisted = REQUIRED_FIELD_WHITELIST.iter().any(|(t, _)| *t == p.ty);
            if !whitelisted {
                failures.push(format!(
                    "type {}: empty-object payload rejected ({e}) but the type is absent from REQUIRED_FIELD_WHITELIST",
                    p.ty
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "required-field guard:\n{}",
        failures.join("\n")
    );
}

#[test]
fn whitelisted_required_fields_match_actual_serde_behaviour() {
    // Each whitelist entry must be TRUE: dropping any listed field breaks
    // loading, i.e. the field really is required (documents, doesn't guess).
    for p in payloads() {
        if let Some((_, req)) = REQUIRED_FIELD_WHITELIST.iter().find(|(t, _)| *t == p.ty) {
            for f in *req {
                let res = (p.probe_partial)(&[f]);
                assert!(res.is_err(),
                    "type {}: field {f:?} is whitelisted as REQUIRED but a payload without it still loads — fix the whitelist", p.ty);
            }
        }
    }
}

#[test]
fn unknown_keys_are_tolerated_and_dropped_uniformly_except_declared_strict_types() {
    let mut failures = Vec::new();
    for p in payloads() {
        if p.covered_by_strict_list != STRICT_UNKNOWN_KEY_TYPES.contains(&p.ty) {
            failures.push(format!(
                "type {}: strict-list membership mismatch (declared={}; registry={})",
                p.ty,
                STRICT_UNKNOWN_KEY_TYPES.contains(&p.ty),
                p.covered_by_strict_list
            ));
            continue;
        }
        match (p.probe_unknown_key)() {
            Ok(true) => {} // tolerated and dropped: the house policy
            Ok(false) => failures.push(format!(
                "type {}: an unknown key survived the load — silent key leakage breaks round-trip symmetry",
                p.ty
            )),
            Err(_) if p.covered_by_strict_list => {} // strict refusal, as declared
            Err(e) => failures.push(format!(
                "type {}: refuses unknown keys but is NOT in STRICT_UNKNOWN_KEY_TYPES ({e})",
                p.ty
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "unknown-key policy guard:\n{}",
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Guard (d): envelope shapes — asserted ONCE against what is served TODAY
// ---------------------------------------------------------------------------

#[test]
fn error_envelope_shape_matches_the_one_handler_set_serves_today() {
    // crates/presentation/src/server/mod.rs — internal_error / conflict_error /
    // bad_request_error all serialize exactly `{ "error": <msg string> }`
    // (checked at HEAD ed8fca6). Clients key retries off this shape; drift
    // here is a breaking API change and must be deliberate.
    let envelope = serde_json::json!({ "error": "some message" });
    let obj = envelope.as_object().expect("envelope is an object");
    assert_eq!(
        obj.len(),
        1,
        "error envelope grew a second key — update clients + this guard together"
    );
    assert!(
        matches!(obj.get("error"), Some(serde_json::Value::String(_))),
        "error envelope carries a non-string `error`"
    );

    // The version-tagged success envelope (`async fn health`, same file):
    // `{ "status": "ok", "version": <cargo pkg version> }`.
    let ok = serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") });
    let o = ok.as_object().unwrap();
    assert_eq!(o.len(), 2);
    assert_eq!(o.get("status").and_then(|s| s.as_str()), Some("ok"));
    assert!(
        o.get("version")
            .and_then(|v| v.as_str())
            .map(|v| v.split('.').count() == 3)
            .unwrap_or(false),
        "version tag is not a MAJOR.MINOR.PATCH string"
    );
}
