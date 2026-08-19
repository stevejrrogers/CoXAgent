//! `RegistryKnowledgePort` — knowing what registries report about a dependency:
//! its latest release and any high-severity CVEs.
//!
//! The dependency-health scanner decides deterministically offline over
//! workspace file contents; "what is latest / what has a CVE" is external
//! knowledge supplied by an adapter through this port as precomputed data —
//! never fetched live from inside the use case on every cycle.
//!
//! An absent/no-knowledge answer means callers SKIP that package rather than
//! guess — which also models "private registry with no credential available".

/// Registry advisory for one package identity.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageAdvisory {
    /// Latest released version exactly as reported; empty when unknown or
    /// private-and-unauthenticated (callers skip rather than flag).
    pub latest_version: String,
    /// Highest reported severity (`"high"`, `"critical"`), empty when none.
    pub cve_severity: String,
}

/// Adapters answer synchronously from an already-fetched snapshot keyed by
/// `(ecosystem, package_name)`.
#[async_trait::async_trait]
pub trait RegistryKnowledgePort: Send + Sync {
    /// Advisory for one dependency identity. `None` = no knowledge; skip.
    async fn resolve(&self, ecosystem: &str, name: &str) -> Option<PackageAdvisory>;
}
