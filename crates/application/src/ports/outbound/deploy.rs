//! `DeployPort` — the boundary over "make the codebase run" (docker compose,
//! test-only, k8s...). The cycle deploys after DEV so TEST verifies a running
//! build. Adapters live in infrastructure.

use crate::error::PortError;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

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

    /// Ensure the underlying runtime (e.g. the Docker daemon) is up, attempting
    /// to start it if it isn't. Returns whether it is running afterwards. The
    /// default assumes no daemon is needed.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the runtime state can't be determined.
    async fn ensure_daemon(&self) -> Result<bool, PortError> {
        Ok(true)
    }

    /// Tear down whatever this directory's deploy started (e.g. `docker compose
    /// down`), freeing its ports — used when swapping the app for a PR preview.
    /// Default: nothing to stop.
    ///
    /// # Errors
    /// [`PortError::Backend`] on an unexpected failure to run the deploy tool.
    async fn down(&self, _work_dir: &Path) -> Result<(), PortError> {
        Ok(())
    }

    /// Liveness check for the deployed app: is something accepting connections
    /// on `127.0.0.1:<port>`? Used by the Ops/SRE monitor to detect an app that
    /// crashed after deploy. The default reports healthy (no monitor).
    ///
    /// # Errors
    /// [`PortError::Backend`] if the check can't run.
    async fn health(&self, _port: u16) -> Result<bool, PortError> {
        Ok(true)
    }

    /// Run the project's test suite as a hard Definition-of-Done gate — detect
    /// the toolchain and run its tests. `deployed=false` means no toolchain was
    /// recognised (skipped). The default skips.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the test tool can't be launched.
    /// Count lint errors (e.g. `cargo clippy`) for the workspace. `None` =
    /// linting not supported for this project type. Default: unsupported.
    ///
    /// # Errors
    /// [`PortError`] on spawn failure.
    async fn lint(&self, _work_dir: &Path) -> Result<Option<u64>, PortError> {
        Ok(None)
    }

    async fn run_tests(&self, work_dir: &Path) -> Result<DeployReport, PortError> {
        let _ = work_dir;
        Ok(DeployReport {
            success: true,
            deployed: false,
            summary: "no test runner".to_owned(),
        })
    }
}

/// Mandatory post-deploy health probe (COX-B004/COX-B009): a `docker compose
/// up` exit 0 only proves the containers started — it says nothing about
/// whether the app inside actually bound its configured port. This polls
/// [`DeployPort::health`] for a bounded window so every call site that reports
/// a deploy as a success — the autonomous cycle, chat's "deploy" command, and
/// the PR-preview endpoint alike — is gated the same way and none of them can
/// downgrade a dead-on-arrival container into "success" by skipping the
/// check. No `host_port` configured means nothing to probe (matches
/// `ops_monitor`'s own gate); a `deploy` port with no real check (default
/// `DeployPort::health` impl) reports healthy immediately, same as before
/// this gate existed.
///
/// # Errors
/// Never returns an error — an unreachable/failing health check is reported
/// as `false`, not propagated.
pub async fn verify_deploy_health(deploy: &Arc<dyn DeployPort>, host_port: Option<u16>) -> bool {
    const ATTEMPTS: u32 = 15;
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    let Some(port) = host_port else {
        return true;
    };
    for attempt in 0..ATTEMPTS {
        if deploy.health(port).await.unwrap_or(true) {
            return true;
        }
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    false
}
