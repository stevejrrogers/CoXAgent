//! `DependencyDiscoveryPort` — the boundary over lockfile discovery for the
//! dependency-health scan (CXA-F009 / CXA-B111). Finding the lock files under a
//! workspace and reading their bodies is IO, so it lives behind this port and
//! its infrastructure adapter; the scanner in [`crate::deps_scan`] stays pure
//! over the `(path, body)` pairs the adapter hands it, and use cases stay
//! testable with an in-memory double.

use crate::error::PortError;
use async_trait::async_trait;
use std::path::Path;

/// One discovered lockfile: where it sits (repo-relative, e.g.
/// `e2e/package-lock.json`) and what it pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    /// Repo-relative display path, as tickets and reports should name it.
    pub path: String,
    /// Full file body.
    pub body: String,
}

#[async_trait]
pub trait DependencyDiscoveryPort: Send + Sync {
    /// Every recognised lockfile under `root` — root and all subdirectories,
    /// vendored/build directories (e.g. `node_modules`, `target`) and VCS or
    /// dotfile state excluded. Recognition follows the scanner's own contract
    /// ([`crate::deps_scan::is_lockfile`]), so what counts as a lock stays
    /// owned by production code, not by the adapter. A missing root reads as
    /// an empty result — an empty workspace has nothing to scan.
    ///
    /// The walk stays inside `root` (CXA-B118): symlinks are never followed —
    /// not descended into, not read through — and the walk is bounded, so a
    /// workspace beyond the bound is an error rather than a hung request.
    ///
    /// # Errors
    /// [`PortError::Backend`] when a discovered lockfile cannot be read, or
    /// when the workspace exceeds the adapter's discovery bound.
    async fn discover_lockfiles(&self, root: &Path) -> Result<Vec<Lockfile>, PortError>;
}
