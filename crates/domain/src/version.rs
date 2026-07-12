//! Semantic version value object with explicit bump semantics.

use crate::error::DomainError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Which part of a semantic version to bump on deploy.
///
/// Mapping (from the plan): bug fix -> `Patch`, new feature -> `Minor`,
/// breaking/architecture change -> `Major`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bump {
    Major,
    Minor,
    Patch,
}

/// A semantic version. Thin newtype over [`semver::Version`] restricted to the
/// `MAJOR.MINOR.PATCH` core the workflow uses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SemVer(semver::Version);

impl SemVer {
    #[must_use]
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self(semver::Version::new(major, minor, patch))
    }

    /// Parse a `MAJOR.MINOR.PATCH` string.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidVersion`] when `raw` is not valid semver.
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        semver::Version::from_str(raw)
            .map(Self)
            .map_err(|_| DomainError::InvalidVersion(raw.to_owned()))
    }

    /// Return a new version with the requested part bumped and lower parts reset.
    #[must_use]
    pub fn bumped(&self, bump: Bump) -> Self {
        let v = &self.0;
        match bump {
            Bump::Major => Self::new(v.major + 1, 0, 0),
            Bump::Minor => Self::new(v.major, v.minor + 1, 0),
            Bump::Patch => Self::new(v.major, v.minor, v.patch + 1),
        }
    }
}

impl Default for SemVer {
    fn default() -> Self {
        Self::new(0, 0, 0)
    }
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_patch_increments_last() {
        let v = SemVer::new(1, 5, 0);
        assert_eq!(v.bumped(Bump::Patch), SemVer::new(1, 5, 1));
    }

    #[test]
    fn bump_minor_resets_patch() {
        let v = SemVer::new(1, 5, 3);
        assert_eq!(v.bumped(Bump::Minor), SemVer::new(1, 6, 0));
    }

    #[test]
    fn bump_major_resets_minor_and_patch() {
        let v = SemVer::new(1, 5, 3);
        assert_eq!(v.bumped(Bump::Major), SemVer::new(2, 0, 0));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(SemVer::parse("not-a-version").is_err());
    }
}
