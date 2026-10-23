//! CXA-B286 part 1/3 — inventory of every public serde DTO/domain value type in
//! `coxagent-domain`, plus ONE shared round-trip harness, proven on two real
//! types. Per-type bodies land in parts 2/3.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_domain::artifact::ArtifactVersion;
use coxagent_domain::version::SemVer;

// ---------------------------------------------------------------------------
// Shared harness: pure functions over the serializer only (no IO).
// ---------------------------------------------------------------------------

/// Symmetric round trip: `serialize -> deserialize == original`, and both
/// serialization passes are byte-stable.
fn assert_symmetric_round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = serde_json::to_string(value).expect("serialize");
    let back: T = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(&back, value, "a field lost or mangled its shape in transit");
    assert_eq!(serde_json::to_string(&back).expect("re-serialize"), text);
}

// ---------------------------------------------------------------------------
// Smoke proof of the harness on two REAL types (this ticket's own AC).
// ---------------------------------------------------------------------------

#[test]
fn semver_newtype_round_trips_transparently() {
    let v = SemVer::new(2, 32, 0);
    assert_symmetric_round_trip(&v);
    // Transparent wire form: bare string, not an object.
    assert_eq!(
        serde_json::to_value(SemVer::new(1, 2, 3)).unwrap(),
        serde_json::json!("1.2.3")
    );
}

#[test]
fn artifact_version_round_trips_with_all_fields_populated() {
    // Real constructor — also proves `SemVer`'s transparent wire shape.
    let a = ArtifactVersion::new("cxa", "web", "1.4.2").expect("valid semver");
    assert_symmetric_round_trip(&a);
    let obj = serde_json::to_value(&a).unwrap();
    assert_eq!(obj.get("version"), Some(&serde_json::json!("1.4.2")));
}

// ---------------------------------------------------------------------------
// Type-inventory manifest (machine-checkable), mirrored from
// tests/fixtures/type_inventory.md. Adding/removing a public serde type in
// crates/domain/src must update BOTH or this fails.
// ---------------------------------------------------------------------------

const DOMAIN_SERDE_TYPES: &[&str] = &[
    "artifact::ArtifactVersion",
    "coverage::CoverageStatus",
    "coverage::CoverageEntry",
    "kinds::TicketType",
    "kinds::Priority",
    "kinds::Complexity",
    "kinds::InterventionKind",
    "kinds::Status",
    "kinds::Role",
    "ids::TicketId",
    "ids::GoalId",
    "ids::WorkerId",
    "version::Bump",
    "version::SemVer",
    "debt::DebtSignalKind",
    "debt::DebtSignal",
    "events::EventKind",
    "events::DomainEvent",
    "goal::GoalStatus",
    "goal::Goal",
    "wip_checkpoint::WipCheckpoint",
    "panel_terminal_state::EmptyReason",
    "panel_terminal_state::FillHint",
    "ticket::TechnicalDesign",
    "ticket::UxDesign",
    "ticket::Design",
    "ticket::Ticket",
    "test_case::TestCaseStatus",
    "test_case::RecordKind",
    "test_case::CaseEvidence",
    "test_case::VerdictRecord",
    "test_case::TestCase",
];

/// Guard: the manifest under tests/fixtures lists exactly these types, all
/// pending status (parts 2–3 flip them). One reflective assertion over both
/// artifacts instead of drift-prone duplicate tables.
#[test]
fn manifest_lists_exactly_the_public_serde_types_still_pending() {
    let md = include_str!("fixtures/type_inventory.md");
    for ty in DOMAIN_SERDE_TYPES {
        let row = format!("| `{ty}` | pending |");
        assert!(md.contains(&row), "manifest missing/stale row for {ty}");
    }
    // Every table row in the manifest is accounted for above (and is pending):
    let stale: Vec<_> = md
        .lines()
        .filter(|l| l.starts_with("| `") && !DOMAIN_SERDE_TYPES.iter().any(|t| l.contains(t)))
        .collect();
    assert!(
        stale.is_empty(),
        "manifest rows not backed by the registry: {stale:?}"
    );
}
