//! The artifact-version registry and its schema anchor (CXA-F224 — the
//! CXA-F223a deliverable, from CXA-F223).
//!
//! Before this unit, artifact versions were scattered — the workspace
//! `[package] version` in the root `Cargo.toml`, the web artifact's version in
//! the root `package.json` — with no single authoritative declaration of which
//! build artifacts exist or which schema their persisted registry carries.
//! Both halves below are pure constants and pure data: consumers read THIS
//! module, never parsing `Cargo.toml`/`package.json` at runtime, and no path
//! here does IO.
//!
//! - [`ArtifactRegistry`] — the build's own manifest: each platform artifact
//!   it ships and the semver it currently carries, derived from pure constants
//!   only.
//! - [`ArtifactsConfig`] — the persisted per-project registry (the
//!   `artifacts` section of `coxagent.json`), anchored by
//!   [`ARTIFACT_SCHEMA_VERSION`] and enforced fail-closed by
//!   [`crate::config_parse::parse_config`], exactly like
//!   [`crate::config::CONFIG_SCHEMA_VERSION`] guards the document: a registry
//!   written by a newer build is refused at load, never silently defaulted.

use coxagent_domain::{ArtifactVersion, SemVer};
use serde::{Deserialize, Serialize};

/// Version of the persisted artifact-registry schema
/// (`artifacts.schema_version`) this build understands.
///
/// A manifest carrying a HIGHER anchor is written by a future build: load
/// refuses it rather than accepting a shape it cannot represent or defaulting
/// it away — the same fail-closed posture as
/// [`crate::config::CONFIG_SCHEMA_VERSION`] for the document and
/// [`crate::state::SCHEMA_VERSION`] for state.json. A section that omits the
/// anchor predates it and loads with the documented defaults.
pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;

/// The rust workspace crate's version, as declared in the root `Cargo.toml`
/// (`[package] version`). Mirrored as a constant so [`ArtifactRegistry`] stays
/// pure — the drift gate pins the manifest to these strings.
pub const WORKSPACE_CRATE_VERSION: &str = "2.28.0";

/// The web desktop artifact's version, as declared in the root `package.json`
/// (`version`). Mirrored as a constant for the same reason.
pub const WEB_DESKTOP_VERSION: &str = "1.0.0";

/// One build artifact this project ships, with the semver it currently
/// carries. Declared, not discovered: the manifest comes from this module's
/// constants, never from `Cargo.toml`/`package.json` at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRegistry {
    /// Stable artifact name (e.g. `"rust-workspace"`).
    pub name: &'static str,
    /// The semantic version the artifact currently carries.
    pub semver_version: SemVer,
}

impl ArtifactRegistry {
    /// The rust workspace crate — the artifact every other one builds from.
    #[must_use]
    pub fn rust_workspace() -> Self {
        Self {
            name: "rust-workspace",
            semver_version: SemVer::new(2, 28, 0),
        }
    }

    /// The web desktop artifact.
    #[must_use]
    pub fn web_desktop() -> Self {
        Self {
            name: "web-desktop",
            semver_version: SemVer::new(1, 0, 0),
        }
    }

    /// The declared manifest: every platform artifact this build ships, in
    /// declaration order.
    #[must_use]
    pub fn declared() -> Vec<Self> {
        vec![Self::rust_workspace(), Self::web_desktop()]
    }
}

/// The persisted per-project artifact-version registry — the `artifacts`
/// section of `coxagent.json`.
///
/// COX-B043: defaults are EXPLICIT — an unset anchor reads as the supported
/// [`ARTIFACT_SCHEMA_VERSION`] and the registry starts empty — never Rust's
/// derived zero-value, which would masquerade an unset document as
/// "schema version 0", a shape no build ever wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactsConfig {
    /// Schema anchor of the persisted registry. A document carrying a higher
    /// anchor is refused at load (fail-closed, see
    /// [`crate::config_parse::parse_config`]), never defaulted away.
    #[serde(default = "default_artifact_schema_version")]
    pub schema_version: u32,
    /// The artifact versions the project has declared so far.
    #[serde(default)]
    pub versions: Vec<ArtifactVersion>,
}

fn default_artifact_schema_version() -> u32 {
    ARTIFACT_SCHEMA_VERSION
}

impl Default for ArtifactsConfig {
    /// The documented-and-true defaults (COX-B043): the anchor sits at the
    /// supported version, the registry starts empty.
    fn default() -> Self {
        Self {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            versions: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_manifest_matches_the_declared_version_constants() {
        // The manifest is compiled from components; the string constants above
        // mirror the root Cargo.toml / package.json. This pin refuses drift
        // between the two representations.
        for (artifact, declared) in [
            (ArtifactRegistry::rust_workspace(), WORKSPACE_CRATE_VERSION),
            (ArtifactRegistry::web_desktop(), WEB_DESKTOP_VERSION),
        ] {
            assert_eq!(
                artifact.semver_version,
                SemVer::parse(declared).expect("declared version is valid semver"),
                "{} must carry the declared version {declared}",
                artifact.name
            );
        }
    }

    #[test]
    fn the_manifest_declares_both_platform_artifacts() {
        let declared = ArtifactRegistry::declared();
        assert_eq!(
            declared.iter().map(|a| a.name).collect::<Vec<_>>(),
            ["rust-workspace", "web-desktop"]
        );
    }

    #[test]
    fn an_unset_registry_reads_as_documented_defaults_not_zero_values() {
        // COX-B043: an unset anchor reads as the SUPPORTED version — never 0,
        // which is what a derived zero-value Default would produce.
        let reg: ArtifactsConfig = serde_json::from_str("{}").expect("empty section");
        assert_eq!(reg, ArtifactsConfig::default());
        assert_eq!(reg.schema_version, ARTIFACT_SCHEMA_VERSION);
        assert!(reg.versions.is_empty());
    }

    #[test]
    fn an_unset_anchor_defaults_to_the_supported_version_but_keeps_declared_entries() {
        // The per-field default wiring: a registry that declares entries but
        // predates the anchor loads with the SUPPORTED anchor — and the
        // declared entries survive verbatim, not re-defaulted away.
        let doc = r#"{"versions":[{"project":"cxa","artifact":"web","version":"1.2.3"}]}"#;
        let reg: ArtifactsConfig = serde_json::from_str(doc).expect("anchorless registry loads");

        assert_eq!(reg.schema_version, ARTIFACT_SCHEMA_VERSION);
        assert_eq!(
            reg.versions,
            vec![ArtifactVersion::new("cxa", "web", "1.2.3").expect("valid semver")]
        );
    }

    #[test]
    fn a_registry_round_trips_through_json() {
        let doc = r#"{"schema_version":1,"versions":[
            {"project":"cxa","artifact":"web","version":"1.2.3"}]}"#;
        let reg: ArtifactsConfig = serde_json::from_str(doc).expect("registry doc loads");

        let saved = serde_json::to_string(&reg).expect("registry serializes");
        let back: ArtifactsConfig = serde_json::from_str(&saved).expect("saved registry loads");
        assert_eq!(reg, back, "values persist across save/load unchanged");
        assert_eq!(back.versions[0].version.to_string(), "1.2.3");
    }
}
