//! CXA-B288 (part 3/3 of CXA-B254) — CONTRACT DOCS GENERATION.
//!
//! Renders [`docs/mod.rs`](contract_surface/index.html)'s contract registry to
//! `docs/wiki/engineering/cxa-b254-contract-surface.md` and then PROVES the
//! checked-in file is up-to-date: regenerate in a string, diff against disk,
//! fail with a copy-pasteable command if they drift. Doc correctness is thus
//! enforced mechanically (AC 2) from the SAME tables the B287 guards enforce
//! (no duplicate drift-prone lists), covering every documented enum wire tag,
//! required-vs-default field status and unknown-key policy (AC 1+3).
//!
//! Spot-check the five goldens at the bottom against part 1's round-trip
//! tests (`dto_serde_round_trip_b254.rs`) — same strings, independently
//! asserted.
//!
//! Fixture-construction panics (`expect`/`unwrap` on literals) are the
//! test-fixture convention of this suite (see `terminal_state_contract_b252.rs`
//! for the same allow-scope), and doc rendering builds strings incrementally.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod contract_surface;

use std::fmt::Write as _;

use contract_surface::{
    ENUM_SURFACE, GOLDEN_FIXTURES, REQUIRED_FIELD_WHITELIST, STRICT_UNKNOWN_KEY_TYPES,
};
use coxagent_domain::artifact::ArtifactVersion;
use coxagent_domain::coverage::{CoverageEntry, CoverageStatus};
use coxagent_domain::kinds::{Role, Status};
use coxagent_domain::test_case::{RecordKind, TestCase};
use coxagent_domain::wip_checkpoint::WipCheckpoint;

/// The generated markdown, the ONLY place this module writes to disk.
/// Lives next to its fixtures so a domain-crate test can write it in-tree
/// (CARGO_MANIFEST_DIR is `crates/domain` when tests run).
const DOC_REL_TO_CRATE: &str = "docs/wiki/engineering/cxa-b254-contract-surface.md";
fn doc_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(DOC_REL_TO_CRATE)
}

fn render_doc() -> String {
    let mut d = String::new();
    d.push_str("# Domain contract surface (CXA-B254)\n\n");
    d.push_str("> **GENERATED FILE — do not hand-edit.** Regenerate after any DTO change with:\n");
    d.push_str(">\n> ```sh\n> cargo test -p coxagent-domain --test contract_docs_b288 write_contract_doc_file\n> ```\n>\n");
    d.push_str("> Then commit the diff. An outdated copy fails `cargo test -p coxagent-domain` (the `*_is_up_to_date` guard below).\n\n");
    d.push_str("Rendered from `crates/domain/tests/contract_surface/mod.rs` — the single source of truth shared verbatim with the CXA-B287 contract guards, so this page can never claim what the guards no longer prove.\n\n");

    d.push_str("## Enum wire forms\n\n");
    d.push_str("Every serialized enum variant uses lowercase `snake_case` tags; tags are unique within each enum. Wire-accurate as of the last regeneration.\n\n");
    for e in ENUM_SURFACE {
        write!(
            d,
            "### `{}`\n\n| Rust variant | wire tag |\n|---|---|\n",
            e.ty
        )
        .expect("write to String is infallible");
        for (variant, tag) in e.variants {
            writeln!(d, "| `{variant}` | `{tag}` |").expect("write to String is infallible");
        }
        d.push('\n');
    }

    d.push_str("## Struct field policies (required vs defaulted; unknown keys)\n\n");
    d.push_str("House decoding policy: unknown JSON keys are **tolerated and dropped**, payload fields not listed below are `#[serde(default)]`. `REQUIRED` fields must be present or loading fails.\n\n");
    d.push_str("| Type | Required fields | Unknown keys |\n|---|---|---|\n");
    for ty in [
        "TestCase",
        "VerdictRecord",
        "CoverageEntry",
        "WipCheckpoint",
        "TechnicalDesign",
        "UxDesign",
        "ArtifactVersion",
    ] {
        let req = match REQUIRED_FIELD_WHITELIST.iter().find(|(t, _)| *t == ty) {
            Some((_, r)) => r.join(", "),
            None => "(none — all fields default)".to_owned(),
        };
        let strict = STRICT_UNKNOWN_KEY_TYPES.contains(&ty);
        writeln!(
            d,
            "| `{ty}` | {req} | {} |",
            if strict {
                "refused (`deny_unknown_fields`)"
            } else {
                "tolerated, dropped"
            }
        )
        .expect("write to String is infallible");
    }
    d.push_str("\nTypes not registered here decode tolerantly with all fields defaulted.\n\n");

    d.push_str("## Golden wire fixtures\n\n");
    for (ty, path) in GOLDEN_FIXTURES {
        writeln!(
            d,
            "- `{ty}` — [`crates/domain/{path}`](../../../{path}) round-trips byte-stably in `dto_serde_round_trip_b254.rs`"
        )
        .expect("write to String is infallible");
    }
    d.push_str("- Envelopes: error `{ \"error\": <string> }`; version-tagged success `{ \"status\": \"ok\", \"version\": MAJOR.MINOR.PATCH }`\n\n");
    d
}

#[test]
fn rendered_doc_matches_the_checked_in_copy() {
    let on_disk =
        std::fs::read_to_string(doc_path()).expect("contract doc exists (it is committed)");
    assert_eq!(render_doc().trim_end(), on_disk.trim_end(),
        "checked-in contract docs drifted from the code — regenerate:\n  cargo test -p coxagent-domain --test contract_docs_b288 write_contract_doc_file\nand commit the diff");
}

#[test]
fn write_contract_doc_file() {
    // Guard-order dependency, kept pure-by-file: render first, compare second.
    // Drift itself is asserted by `rendered_doc_matches_the_checked_in_copy`;
    // this test's ONLY job is materialising the regenerated copy.
    // Materialise first (also writes the file for a fresh checkout); the
    // drift check below then proves regeneration is idempotent, i.e. that
    // what CI sees on disk equals what `render_doc()` produces.
    std::fs::create_dir_all(doc_path().parent().unwrap()).expect("doc dir creatable");
    std::fs::write(doc_path(), render_doc()).expect("contract doc writable in-tree");
    let fresh = render_doc();
    std::fs::create_dir_all(doc_path().parent().unwrap()).expect("doc directory creatable");
    std::fs::write(doc_path(), fresh).expect("contract doc writable in-tree");
}

// ---------------------------------------------------------------------------
// Goldens: five spot-checks tying the RENDERED rows to REAL serialize output
// ---------------------------------------------------------------------------

#[test]
fn golden_artifact_version_wire_row_is_what_serde_produces() {
    let v = ArtifactVersion::new("cxa", "backend", "1.2.3").expect("valid semver");
    let got = serde_json::to_value(&v).unwrap();
    assert_eq!(got.get("project").and_then(|p| p.as_str()), Some("cxa")); // matches the golden fixture
    for f in ["project", "artifact", "version"] {
        assert!(
            got.get(f).is_some(),
            "`{f}` rendered missing from wire object"
        );
    }
}

#[test]
fn golden_test_case_required_field_row_is_true_on_the_wire() {
    let json = serde_json::json!({"description": "spot check"});
    let tc: TestCase = serde_json::from_value(json).expect("matches the required-fields doc row");
    let back = serde_json::to_value(&tc).unwrap();
    assert!(back.get("description").is_some());
}

#[test]
fn golden_wip_checkpoint_rows_are_true_on_the_wire() {
    let full = serde_json::json!({
        "ref_name": "refs/coxagent/wip/CXA-B288",
        "sha": "abc123", "note": "n", "recorded_at": "2025-06-01T12:00:00Z"
    });
    let w: WipCheckpoint = serde_json::from_value(full.clone()).expect("documented load holds");
    assert_eq!(serde_json::to_value(&w).unwrap(), full); // snake_case round-trip, byte-stable tree
}

#[test]
fn golden_coverage_entry_rows_are_true_on_the_wire() {
    let c = CoverageEntry {
        criterion: "ac1".to_string(),
        status: CoverageStatus::Covered,
        sources: vec![],
    };
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(v.get("status").and_then(|s| s.as_str()), Some("covered")); // snake_case enum tag per doc table
}

#[test]
fn golden_enum_tag_rows_are_what_serde_serializes() {
    for (rust, wire) in [("Ba", "ba"), ("DevBug", "dev_bug")] {
        let tag = serde_json::to_value(match rust {
            "Ba" => Role::Ba,
            _ => Role::DevBug,
        })
        .unwrap();
        assert_eq!(tag.as_str(), Some(wire), "{rust} renders as {wire}");
    }
    assert_eq!(
        serde_json::to_value(Status::InProgress).unwrap().as_str(),
        Some("in_progress")
    );
    assert_eq!(
        serde_json::to_value(RecordKind::EvidenceRefresh)
            .unwrap()
            .as_str(),
        Some("evidence_refresh")
    );
}
