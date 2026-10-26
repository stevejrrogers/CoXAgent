//! CXA-B286 (part 1/3 of CXA-B254) — serialization round-trips for every
//! public boundary-crossing DTO/domain value type in `coxagent-domain`.
//!
//! Each type gets two proofs over its REAL constructors (no fabricated data):
//!
//! 1. symmetric round-trip: `serialize -> deserialize == original`, AND
//!    `deserialize -> serialize` byte-stable (value equality of both trees),
//! 2. a wire-shape assertion pinning the structural keys of the
//!    representative wire-format types (`serde_json`) against the checked-in
//!    goldens in [`fixtures`](fixtures/) — so a rename or dropped field fails
//!    loudly instead of silently changing the persisted shape.
//!
//! Asymmetry policy: where a field is skipped/defaulted (Option fields,
//! defaulted lists) symmetry is proven by populating EVERY field on the
//! outbound side, then also proving the defaulted load path keeps the same
//! value — never by weakening an assertion.
//!
//! Fixture-construction panics (`expect`/`unwrap` on serde round-trips of
//! literal fixtures) are the test-fixture convention of this suite (same
//! allow-scope as `terminal_state_contract_b252.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_domain::artifact::ArtifactVersion;
use coxagent_domain::coverage::{CoverageEntry, CoverageStatus};
use coxagent_domain::ids::{GoalId, TicketId};
use coxagent_domain::kinds::{Complexity, Priority, Role, Status, TicketType};
use coxagent_domain::test_case::{
    CaseEvidence, RecordKind, TestCase, TestCaseStatus, VerdictRecord,
};
use coxagent_domain::ticket::{Design, TechnicalDesign, Ticket, UxDesign};
use coxagent_domain::version::SemVer;
use coxagent_domain::wip_checkpoint::WipCheckpoint;

const WHEN_RFC3339: &str = "2025-06-01T12:00:00Z";

// ---------------------------------------------------------------------------
// ids — newtype identifiers persist as their bare strings
// ---------------------------------------------------------------------------

#[test]
fn id_newtypes_round_trip_as_bare_strings() {
    let tid = TicketId::new("CXC-F001").expect("valid ticket id");
    let v = serde_json::to_value(&tid).unwrap();
    assert_eq!(v, serde_json::json!("CXC-F001"));
    let back: TicketId = serde_json::from_value(v).unwrap();
    assert_eq!(back, tid);

    let gid = GoalId::new("G-42").expect("valid goal id");
    let gv = serde_json::to_value(&gid).unwrap();
    assert_eq!(gv, serde_json::json!("G-42"));
    let gback: GoalId = serde_json::from_value(gv).unwrap();
    assert_eq!(gback, gid);
}

// ---------------------------------------------------------------------------
// kinds — enums cross the wire as snake_case tags
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_kinds_serialize_to_snake_case_and_back() {
    let all: [(serde_json::Value, &dyn std::any::Any); 0] = [];
    let _ = all; // placeholder removed below

    // Wire shapes pinned: any rename shows up here first.
    for (wire, ty) in [
        ("feature", TicketType::Feature),
        ("bug", TicketType::Bug),
        ("chore", TicketType::Chore),
    ] {
        assert_eq!(serde_json::to_value(ty).unwrap(), serde_json::json!(wire));
        let back: TicketType = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(back, ty);
    }
    for (wire, st) in [
        ("pending", Status::Pending),
        ("in_progress", Status::InProgress),
        ("on_hold", Status::OnHold),
        ("documented", Status::Documented),
        ("verified", Status::Verified),
    ] {
        assert_eq!(serde_json::to_value(st).unwrap(), serde_json::json!(wire));
        let back: Status = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(back, st);
    }
    for (wire, r) in [
        ("ba", Role::Ba),
        ("po", Role::Po),
        ("dev_bug", Role::DevBug),
        ("dev_feature", Role::DevFeature),
        ("system", Role::System),
    ] {
        assert_eq!(serde_json::to_value(r).unwrap(), serde_json::json!(wire));
        let back: Role = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(back, r);
    }
    for (wire, p) in [("low", Priority::Low), ("high", Priority::High)] {
        assert_eq!(serde_json::to_value(p).unwrap(), serde_json::json!(wire));
        let back: Priority = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(back, p);
    }
    for (wire, c) in [("small", Complexity::Small), ("large", Complexity::Large)] {
        assert_eq!(serde_json::to_value(c).unwrap(), serde_json::json!(wire));
        let back: Complexity = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(back, c);
    }
}

// ---------------------------------------------------------------------------
// version / artifact identity
// ---------------------------------------------------------------------------

#[test]
fn semver_round_trips_through_its_plain_string_shape() {
    let sv = SemVer::new(7, 11, 3);
    let v = serde_json::to_value(&sv).unwrap();
    assert_eq!(v, serde_json::json!("7.11.3"));
    let back: SemVer = serde_json::from_value(v).unwrap();
    assert_eq!(back, sv);
}

#[test]
fn artifact_version_survives_a_symmetric_json_round_trip() {
    let av = ArtifactVersion::new("cxa", "web", "2.5.0").expect("valid semver");
    let text = serde_json::to_string(&av).unwrap();
    let back: ArtifactVersion = serde_json::from_str(&text).unwrap();
    assert_eq!(back, av);
    assert_eq!(serde_json::to_string(&back).unwrap(), text);
}

#[test]
fn artifact_version_keeps_the_checked_in_wire_keys() {
    let av = ArtifactVersion::new("cxa", "api", "0.1.0").expect("valid semver");
    let got = serde_json::to_value(&av).unwrap();
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/artifact_version_cxa_api.json")).unwrap();
    assert_eq!(
        got, golden,
        "ArtifactVersion wire shape drifted — update the golden deliberately, not silently"
    );
}

// ---------------------------------------------------------------------------
// design value objects + the aggregate itself
// ---------------------------------------------------------------------------

fn a_technical_design() -> TechnicalDesign {
    TechnicalDesign {
        approach: "port a facade over the existing store".into(),
        files: vec!["src/store/mod.rs".into()],
        api_contract: "POST /store/{key}".into(),
        data_changes: "none".into(),
        test_plan: "contract gate + e2e".into(),
        alternatives: "rewrite rejected: blast radius too large".into(),
    }
}

#[test]
fn technical_design_round_trips_with_all_fields_populated() {
    let d = a_technical_design();
    let text = serde_json::to_string(&d).unwrap();
    let back: TechnicalDesign = serde_json::from_str(&text).unwrap();
    assert_eq!(back, d);
    assert_eq!(serde_json::to_string(&back).unwrap(), text);
    // Symmetry guard: `alternatives` carries #[serde(default)]; when present
    // it MUST survive both directions (populate-everything round-trip).
    assert!(serde_json::to_value(&d)
        .unwrap()
        .get("alternatives")
        .is_some());
}

#[test]
fn ux_design_and_design_envelope_round_trip() {
    let ux = UxDesign {
        user_flow: "claim -> work -> done".into(),
        screens: vec!["board".into()],
        component_states: vec!["loading".into()],
        responsive_notes: "single column".into(),
    };
    let envelope = Design {
        technical: Some(a_technical_design()),
        ux: Some(ux.clone()),
    };
    let text = serde_json::to_string(&envelope).unwrap();
    let back: Design = serde_json::from_str(&text).unwrap();
    assert_eq!(back.technical.as_ref(), Some(&a_technical_design()));
    assert_eq!(back.ux, Some(ux));
    assert_eq!(serde_json::to_string(&back).unwrap(), text);
}

/// A fully populated ticket, assembled ONLY through real constructors and
/// guarded mutations — no hand-built struct literals of private fields.
fn a_fully_populated_ticket() -> Ticket {
    let mut t = Ticket::new(
        TicketId::new("FEAT-042").expect("id"),
        TicketType::Feature,
        "Serialization contract guard",
        "Prove the wire shape survives persistence.",
        Priority::High,
        Complexity::Medium,
        true,
    )
    .expect("title is non-blank");

    t.set_priority(Role::Po, Priority::Low)
        .expect("PO may set priority");
    t.set_technical_design(Role::Sa, a_technical_design())
        .expect("SA may attach design");
    t.set_acceptance_criteria(vec![
        "every DTO round-trips".to_string(),
        "goldens stay stable".to_string(),
    ]);
    t.record_wip_checkpoint(
        Role::System,
        WipCheckpoint {
            ref_name: "refs/coxagent/wip/CXA-F318".into(),
            sha: "0ae394e3deadbeefcafe".into(),
            note: "wip/b286 | 3 files changed".into(),
            recorded_at: WHEN_RFC3339.into(),
        },
    )
    .expect("System records checkpoints");
    t
}

#[test]
fn every_persisted_ticket_field_survives_a_serde_round_trip() {
    let t = a_fully_populated_ticket();
    let text = serde_json::to_string(&t).unwrap();
    let back: Ticket = serde_json::from_str(&text).unwrap();
    assert_eq!(back, t, "a field lost or mangled its shape in transit");
    assert_eq!(serde_json::to_string(&back).unwrap(), text);
    // Every top-level key we claim to persist really is on the wire — no
    // silent skip_serializing drift while every slot is occupied.
    let obj = serde_json::to_value(&t).unwrap();
    for key in [
        "id",
        "type",
        "title",
        "description",
        "priority",
        "complexity",
        "status",
        "has_ui",
        "design",
        "acceptance_criteria",
        "test_cases",
        "wip_checkpoints",
    ] {
        assert!(obj.get(key).is_some(), "missing wire key `{key}`");
    }
    assert_eq!(obj.get("type"), Some(&serde_json::json!("feature")));
}

#[test]
fn wip_checkpoint_value_object_round_trips_standalone() {
    let cp = WipCheckpoint {
        ref_name: "refs/coxagent/wip/X".into(),
        sha: "abc123".into(),
        note: "note".into(),
        recorded_at: WHEN_RFC3339.into(),
    };
    let text = serde_json::to_string(&cp).unwrap();
    let back: WipCheckpoint = serde_json::from_str(&text).unwrap();
    assert_eq!(back, cp);
    assert_eq!(serde_json::to_string(&back).unwrap(), text);
}

// ---------------------------------------------------------------------------
// test cases — status, evidence, capped history
// ---------------------------------------------------------------------------

fn a_verified_case() -> TestCase {
    TestCase {
        description: "round trip preserves the verdict history".into(),
        status: TestCaseStatus::Passed,
        evidence: Some(CaseEvidence {
            image: Some("/media/cases/1.png".into()),
            note: Some("verified by TEST on the deployed hub".into()),
            repro: Some("http://127.0.0.1/api/x".into()),
            at: WHEN_RFC3339.into(),
        }),
        sources: vec!["tests/dto_serde_round_trip_b254.rs".into()],
        history: vec![
            VerdictRecord {
                kind: RecordKind::Verdict,
                status: TestCaseStatus::Failed,
                note: Some("first pass found asymmetry".into()),
                image: None,
                at: WHEN_RFC3339.into(),
            },
            VerdictRecord {
                kind: RecordKind::EvidenceRefresh,
                status: TestCaseStatus::Passed,
                note: None,
                image: Some("/media/cases/1.png".into()),
                at: WHEN_RFC3339.into(),
            },
        ],
    }
}

#[test]
fn test_case_with_history_and_evidence_round_trips() {
    let tc = a_verified_case();
    let text = serde_json::to_string(&tc).unwrap();
    let back: TestCase = serde_json::from_str(&text).unwrap();
    assert_eq!(back, tc);
    assert_eq!(serde_json::to_string(&back).unwrap(), text);
}

#[test]
fn test_case_defaults_read_back_symmetrically_when_fields_are_omitted() {
    // The load path a pre-matrix snapshot takes: status/evidence/sources/
    // history omitted. It must land exactly on a freshly-synced Pending case
    // so old tickets survive with no value change.
    let minimal = serde_json::json!({ "description": "criterion only" });
    let back: TestCase = serde_json::from_value(minimal).unwrap();
    assert_eq!(back.description, "criterion only");
    assert_eq!(back.status, TestCaseStatus::Pending);
    assert!(back.evidence.is_none() && back.sources.is_empty() && back.history.is_empty());
    let rt: TestCase = serde_json::from_str(&serde_json::to_string(&back).unwrap()).unwrap();
    assert_eq!(rt, back);
}

#[test]
fn test_case_status_kinds_keep_their_snake_case_tags() {
    for (wire, s) in [
        ("pending", TestCaseStatus::Pending),
        ("passed", TestCaseStatus::Passed),
        ("failed", TestCaseStatus::Failed),
    ] {
        assert_eq!(serde_json::to_value(s).unwrap(), serde_json::json!(wire));
        let back: TestCaseStatus = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(back, s);
    }
    for (wire, k) in [
        ("verdict", RecordKind::Verdict),
        ("evidence_refresh", RecordKind::EvidenceRefresh),
    ] {
        assert_eq!(serde_json::to_value(k).unwrap(), serde_json::json!(wire));
        let back: RecordKind = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(back, k);
    }
}

// ---------------------------------------------------------------------------
// coverage matrix
// ---------------------------------------------------------------------------

fn a_coverage_entry(status: CoverageStatus) -> CoverageEntry {
    CoverageEntry {
        criterion: "the evidence link survives a reload".to_string(),
        status,
        sources: vec!["crates/app/tests/rest_state_store_contract.rs".to_string()],
    }
}

#[test]
fn coverage_entry_round_trips_and_empty_sources_default_back_in() {
    let e = a_coverage_entry(CoverageStatus::Covered);
    let text = serde_json::to_string(&e).unwrap();
    let back: CoverageEntry = serde_json::from_str(&text).unwrap();
    assert_eq!(back, e);
    // defaulted field: an outbound-omitted `sources` list defaults on load
    let obj = serde_json::to_value(a_coverage_entry(CoverageStatus::NotTested)).unwrap();
    assert!(obj.get("sources").is_some());
    let bare: CoverageEntry =
        serde_json::from_value(serde_json::json!({"criterion":"ac1","status":"not_tested"}))
            .unwrap();
    assert_eq!(bare.sources, Vec::<String>::new());
}

#[test]
fn coverage_status_wire_casing_is_snake_case_end_to_end() {
    for (wire, st) in [
        ("covered", CoverageStatus::Covered),
        ("partially_covered", CoverageStatus::PartiallyCovered),
        ("not_tested", CoverageStatus::NotTested),
    ] {
        assert_eq!(serde_json::to_value(st).unwrap(), serde_json::json!(wire));
        let back: CoverageStatus = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(back, st);
    }
}
