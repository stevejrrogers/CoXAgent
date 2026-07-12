//! `DeployPort` — the boundary over "make the codebase run" (docker compose,
//! test-only, k8s...). The cycle deploys after DEV so TEST verifies a running
//! build. Adapters live in infrastructure.

use crate::error::PortError;
use async_trait::async_trait;
use std::path::Path;

/// Outcome of a deploy attempt.
#[derive(Debug, Clone)]
pub struct DeployReport {
    /// Whether the deploy command succeeded (or there was nothing to deploy).
    pub success: bool,
    /// Whether an actual deploy ran (false = skipped, e.g. no compose file).
    pub deployed: bool,
    /// A short human summary for the activity log.
    pub summary: String,
}

/// Deploys the codebase so it can be tested/served.
#[async_trait]
pub trait DeployPort: Send + Sync {
    /// Deploy the project in `work_dir`.
    ///
    /// # Errors
    /// [`PortError::Backend`] on an unexpected failure to run the deploy tool.
    async fn deploy(&self, work_dir: &Path) -> Result<DeployReport, PortError>;
}
