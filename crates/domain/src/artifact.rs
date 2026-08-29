//! Artifact identity — which project ships which artifact, at which version.

use crate::error::DomainError;
use crate::version::SemVer;
use serde::{Deserialize, Serialize};

/// One artifact a project ships, with the semantic version it currently
/// carries. The (`project`, `artifact`) pair is the identity; the version is
/// a validated [`SemVer`], and construction is the only door in — so an
/// invalid version can never enter a registry (CXA-F224).
///
/// The version reuses [`SemVer`]'s own serde shape (`"M.m.p"` as a plain
/// string), so a persisted registry reads back exactly what was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    /// The project the artifact belongs to (e.g. `"cxa"`).
    pub project: String,
    /// The artifact within the project (e.g. `"web"`).
    pub artifact: String,
    /// The semantic version the artifact currently carries.
    pub version: SemVer,
}

impl ArtifactVersion {
    /// Build one from its raw parts, validating the version.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidVersion`] — [`SemVer::parse`]'s own
    /// rejection, naming the raw input — when `version` is not valid semver.
    pub fn new(project: &str, artifact: &str, version: &str) -> Result<Self, DomainError> {
        Ok(Self {
            project: project.to_owned(),
            artifact: artifact.to_owned(),
            version: SemVer::parse(version)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_version_constructs_and_carries_identity() {
        let av = ArtifactVersion::new("cxa", "web", "1.2.3").expect("valid semver");
        assert_eq!(av.project, "cxa");
        assert_eq!(av.artifact, "web");
        assert_eq!(av.version, SemVer::new(1, 2, 3));
    }

    #[test]
    fn an_invalid_version_is_rejected_naming_the_input() {
        // The rejection is SemVer's own — that match is what "reusing SemVer"
        // means.
        let err = ArtifactVersion::new("cxa", "web", "not-a-version").expect_err("invalid semver");
        assert_eq!(err, DomainError::InvalidVersion("not-a-version".to_owned()));
    }

    #[test]
    fn the_version_serializes_as_a_plain_semver_string() {
        let av = ArtifactVersion::new("cxa", "web", "1.2.3").expect("valid semver");
        let json = serde_json::to_value(&av).expect("serialize");
        assert_eq!(json["project"], "cxa");
        assert_eq!(json["artifact"], "web");
        assert_eq!(json["version"], "1.2.3");

        let back: ArtifactVersion = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, av, "the value object round-trips unchanged");
    }
}
