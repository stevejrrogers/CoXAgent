//! TDD tests for CXA-F224 — CXA-F223a: Declare artifact-version registry and
//! schema anchor (from CXA-F223).
//!
//! These tests encode the ticket's acceptance criteria EXACTLY and fail until
//! the behaviour exists:
//!
//! - AC1: "An `ArtifactVersion` value object exists in crates/domain with
//!   project + artifact identity and a validated semver, reusing `SemVer`;
//!   constructing one rejects an invalid version."
//! - AC2: "A persisted per-project artifact-version registry deserializes
//!   from config/state with an EXPLICIT container Default — unset fields read
//!   as documented-and-true, not Rust's zero-value."
//! - AC3: "Omitting the artifacts section entirely loads fine with defaults
//!   (matches every other #[serde(default)] section)."
//! - AC4: "A document carrying a newer-than-supported artifact schema version
//!   fails load with a ConfigParseError naming the field (fail-closed), never
//!   silently defaulted."
//! - AC5: "Round-trip tests prove values persist across save/load unchanged;
//!   defaults are asserted explicitly."
//!
//! WHY THE REGISTRY IS A CONFIG SECTION (grounded, not guessed): AC4 demands a
//! [`ConfigParseError`], and the only producer of that type is
//! `parse_config` — the `coxagent.json` load path. ProjectState load failures
//! are `PortError::Corrupt`, a different type. AC3's "every other
//! #[serde(default)] section" is Config-section language (engine, policy,
//! deploy, …). So the registry is the `artifacts` section of [`Config`].
//!
//! NAMES, AND WHERE THEY COME FROM: the criteria name `ArtifactVersion`
//! (domain, re-exported at the crate root like `SemVer`), the `artifacts`
//! section, and the `schema_version` field whose dotted path
//! `artifacts.schema_version` AC4 requires the error to name. Two names are
//! completed by house convention rather than the criteria's words, and the
//! tests pin them so implementer and reviewer share one contract:
//! the section type is `ArtifactsConfig` (every Config section type is
//! `<Section>Config`: `DeployConfig`, `ReleasesConfig`, `CoverageConfig`),
//! holding the registry as `versions: Vec<ArtifactVersion>` (the criteria's
//! "artifact-version registry" content). No other semantics are assumed.
//!
//! REQUIRED SURFACE this suite compiles against (all house conventions):
//! - `coxagent_domain::ArtifactVersion`: `new(project, artifact, version:
//!   &str) -> Result<Self, DomainError>` (validation must see raw input, so
//!   the version arrives as a string and is parsed via `SemVer`); derives
//!   `Serialize + Deserialize` (transparent `"M.m.p"` for the semver, matching
//!   `SemVer`'s own serde), `PartialEq + Debug` (as every domain VO does).
//! - `coxagent_application::config::ArtifactsConfig`: fields `schema_version:
//!   u32` and `versions: Vec<ArtifactVersion>`; an EXPLICIT (non-derived)
//!   `Default`; `Serialize + Deserialize + PartialEq + Debug`; carried on
//!   `Config` as `#[serde(default)] pub artifacts`.
//!
//! RED STATE: the types do not exist yet, so this target fails to compile —
//! for a declare-a-type ticket the unresolved type IS the missing behaviour,
//! exactly as a failing assertion is for a behaviour inside an existing type.
//! Once the surface above lands, the target compiles and every test below
//! fails only if the behaviour it pins is wrong or missing.
//!
//! THE ANCHOR: "supported" starts at 1 (a new anchor's first supported version
//! — the same posture as `CONFIG_SCHEMA_VERSION`), so the failing document
//! carries 2. Fail-closed means the load RETURNS the error: an implementation
//! that silently defaulted the section would return `Ok` and these tests fail.
//!
//! Pure functions over the domain/config types — no IO, no server, no port,
//! no fixture that any existing type cannot build.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::config::ArtifactsConfig;
use coxagent_application::parse_config;
use coxagent_domain::{ArtifactVersion, DomainError};

/// A minimal valid document that omits every defaulted section — the same
/// shape the existing `config_parse` tests prove loads. Used by AC3.
const ENGINE_ONLY_DOC: &str = r#"{"engine":{"default":{"engine":"claude","model":"sonnet"}}}"#;

/// The registry fixture: one artifact version, written exactly as the
/// persistence layer must read it back (`schema_version` at the supported
/// anchor, the entry carrying project + artifact identity and the semver).
const REGISTRY_DOC: &str = r#"{
  "schema_version": 1,
  "versions": [
    { "project": "cxa", "artifact": "web", "version": "1.2.3" }
  ]
}"#;

// -------------------------------------------------------------------------
// AC1 — the domain value object
// -------------------------------------------------------------------------

#[test]
fn ac1_an_artifact_version_carries_project_artifact_identity_and_a_validated_semver() {
    let av =
        ArtifactVersion::new("cxa", "web", "1.2.3").expect("a valid artifact version constructs");

    // Identity + validated semver, observable through the persistence surface
    // the registry serializes: field names are the criteria's own words.
    let json = serde_json::to_value(&av).expect("the value object serializes");
    assert_eq!(json["project"], "cxa", "project identity is carried");
    assert_eq!(json["artifact"], "web", "artifact identity is carried");
    assert_eq!(
        json["version"], "1.2.3",
        "the semver is carried as MAJOR.MINOR.PATCH"
    );
}

#[test]
fn ac1_constructing_an_artifact_version_rejects_an_invalid_version() {
    let err = ArtifactVersion::new("cxa", "web", "not-a-version")
        .expect_err("an invalid version must be rejected at construction");

    // The rejection is SemVer's own (`DomainError::InvalidVersion` is what
    // `SemVer::parse` returns) — that match is what "reusing SemVer" means.
    assert!(
        matches!(err, DomainError::InvalidVersion(ref raw) if raw == "not-a-version"),
        "expected the SemVer parse rejection naming the input, got: {err:?}"
    );
}

// -------------------------------------------------------------------------
// AC2 — explicit container Default, not Rust's zero-value
// -------------------------------------------------------------------------

#[test]
fn ac2_an_unset_registry_deserializes_to_documented_defaults_not_zero_values() {
    let reg: ArtifactsConfig =
        serde_json::from_str("{}").expect("an empty registry section deserializes");

    // The documented-and-true defaults, asserted explicitly: the anchor sits
    // at the supported version (1) — NOT 0, which is what a derived
    // zero-value Default would produce — and the registry starts empty.
    assert_eq!(
        reg.schema_version, 1,
        "an unset anchor reads as the supported version, never 0"
    );
    assert!(
        reg.versions.is_empty(),
        "an unset registry holds no entries"
    );

    // The container Default IS those same documented values.
    assert_eq!(
        reg,
        ArtifactsConfig::default(),
        "Default matches the documented defaults"
    );
}

// -------------------------------------------------------------------------
// AC3 — omitting the artifacts section loads with defaults
// -------------------------------------------------------------------------

#[test]
fn ac3_a_document_omitting_the_artifacts_section_loads_with_the_registry_defaults() {
    let cfg = parse_config(ENGINE_ONLY_DOC).expect("a document without artifacts loads");

    assert_eq!(
        cfg.artifacts,
        ArtifactsConfig::default(),
        "the omitted section reads as its defaults, like every other #[serde(default)] section"
    );
}

// -------------------------------------------------------------------------
// AC4 — the schema anchor is fail-closed
// -------------------------------------------------------------------------

#[test]
fn ac4_a_newer_than_supported_artifact_schema_version_fails_load_naming_the_field() {
    let err = parse_config(r#"{"artifacts":{"schema_version":2,"versions":[]}}"#)
        .expect_err("a registry written by a newer build must not load");

    // Fail-closed: the load returned an error naming the offending field —
    // never a silent default (a defaulted load would be Ok and fail this test).
    assert_eq!(
        err.field, "artifacts.schema_version",
        "the error names the artifact schema field, got: {err}"
    );
}

#[test]
fn ac4_the_supported_artifact_schema_version_still_loads() {
    // Boundary of "newer-than-supported": the anchor refuses only versions
    // ABOVE the supported one — a current document must keep loading, with
    // the value it carried (not a default) after the load.
    let cfg = parse_config(r#"{"artifacts":{"schema_version":1,"versions":[]}}"#)
        .expect("the supported artifact schema version loads");

    assert_eq!(
        cfg.artifacts.schema_version, 1,
        "the carried anchor survives the load"
    );
}

// -------------------------------------------------------------------------
// AC5 — round-trip persistence, defaults explicit
// -------------------------------------------------------------------------

#[test]
fn ac5_registry_values_round_trip_save_load_unchanged() {
    let loaded: ArtifactsConfig =
        serde_json::from_str(REGISTRY_DOC).expect("the registry document loads");

    // The persisted value IS the domain value: what the document carries is
    // what the value object constructs — identity and semver unchanged.
    assert_eq!(loaded.versions.len(), 1, "the registry holds the one entry");
    assert_eq!(
        loaded.versions[0],
        ArtifactVersion::new("cxa", "web", "1.2.3").expect("a valid artifact version"),
        "the loaded entry equals the domain-constructed value"
    );
    assert_eq!(loaded.schema_version, 1, "the anchor persists unchanged");

    // save/load round trip: serialize (save) then deserialize (load) and the
    // values come back exactly as they went in.
    let saved = serde_json::to_string(&loaded).expect("the registry serializes");
    let reloaded: ArtifactsConfig =
        serde_json::from_str(&saved).expect("the saved registry loads back");
    assert_eq!(
        loaded, reloaded,
        "values persist across save/load unchanged"
    );
}

#[test]
fn ac5_registry_defaults_round_trip_and_are_asserted_explicitly() {
    // The defaults, serialized and loaded back, are still the defaults —
    // asserted against the EXPLICIT container Default, value by value.
    let saved = serde_json::to_string(&ArtifactsConfig::default()).expect("defaults serialize");
    let reloaded: ArtifactsConfig =
        serde_json::from_str(&saved).expect("the default registry loads back");

    assert_eq!(
        reloaded,
        ArtifactsConfig::default(),
        "defaults persist across save/load"
    );
    assert_eq!(
        reloaded.schema_version, 1,
        "the defaulted anchor is the supported version"
    );
    assert!(
        reloaded.versions.is_empty(),
        "the defaulted registry is empty"
    );
}
