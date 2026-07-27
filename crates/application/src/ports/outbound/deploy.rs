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

/// Lint gate measurement: the error count plus a bounded sample of the actual
/// error lines, so repair prompts can name the offending lints.
#[derive(Debug, Clone)]
pub struct LintReport {
    /// Number of lint errors in the workspace.
    pub errors: u64,
    /// Up to a few of the raw error lines (may be empty when unsupported).
    pub sample: String,
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

    /// Detailed health-endpoint probe (COX-F005): a check against the app's
    /// health endpoint on `port`, bounded by a timeout, capturing pass/fail,
    /// HTTP status, and response time — unlike [`Self::health`], which only
    /// reports TCP liveness as a bare bool. An unreachable endpoint
    /// (connection refused, DNS failure) or a probe that exceeds the bound
    /// must be reported as `passed: false`, never left hanging and never
    /// propagated as an error.
    ///
    /// The default wraps [`Self::health`] so adapters that don't override
    /// this (e.g. test doubles, `ScriptedDeploy`-style fakes) get a
    /// zero-behavior-change result: no HTTP status (the TCP check can't see
    /// one), timing from the wrapped call.
    async fn health_check(&self, port: u16) -> crate::state::HealthCheckResult {
        let start = std::time::Instant::now();
        let passed = self.health(port).await.unwrap_or(false);
        crate::state::HealthCheckResult {
            passed,
            http_status: None,
            response_time_ms: Some(u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)),
        }
    }

    /// The mandatory post-deploy gate (COX-F005): poll [`Self::health_check`]
    /// every 2s until it passes or `timeout` elapses, returning the result of
    /// the poll that decided the outcome.
    ///
    /// Polling — not a single probe — is what makes this a usable gate: `docker
    /// compose up` returns as soon as the containers *start*, seconds before
    /// the app inside binds its port, so one immediate probe would fail
    /// perfectly healthy deploys. `timeout` bounds the wait, so an endpoint
    /// that never answers resolves to `passed: false` rather than hanging, and
    /// the last failing probe's HTTP status and timing survive for the deploy
    /// history.
    async fn wait_healthy(
        &self,
        port: u16,
        timeout: std::time::Duration,
    ) -> crate::state::HealthCheckResult {
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let result = self.health_check(port).await;
            if result.passed {
                return result;
            }
            // Stop when another poll couldn't finish inside the bound; the
            // caller gets this probe's detail rather than a bare timeout.
            if tokio::time::Instant::now() + POLL_INTERVAL >= deadline {
                return result;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
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

    /// Like [`Self::lint`] but with a sample of the actual error lines, so a
    /// repair agent sees WHICH lints it introduced instead of a bare count.
    /// Default adapts `lint` with an empty sample.
    ///
    /// # Errors
    /// [`PortError`] on spawn failure.
    async fn lint_report(&self, work_dir: &Path) -> Result<Option<LintReport>, PortError> {
        Ok(self.lint(work_dir).await?.map(|errors| LintReport {
            errors,
            sample: String::new(),
        }))
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
/// Never returns an error — an unreachable/failing health check, and a probe
/// that errors outright, are both reported as `false`, not propagated.
pub async fn verify_deploy_health(deploy: &Arc<dyn DeployPort>, host_port: Option<u16>) -> bool {
    const ATTEMPTS: u32 = 15;
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    let Some(port) = host_port else {
        return true;
    };
    for attempt in 0..ATTEMPTS {
        // A probe that can't run is NOT evidence the app is up: treat it as
        // unhealthy, exactly like `DeployPort::health_check`'s own default.
        // Erring the other way would hand every caller a way to skip the gate
        // by failing the check itself.
        if deploy.health(port).await.unwrap_or(false) {
            return true;
        }
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{DeployPort, DeployReport};
    use crate::error::PortError;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Only `health()` is scripted — `wait_healthy` must work off the trait's
    /// own default `health_check`, which is what every adapter that hasn't
    /// been taught a richer probe still gets.
    struct FakeDeploy {
        healthy: bool,
        probes: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl DeployPort for FakeDeploy {
        async fn deploy(&self, _work_dir: &Path) -> Result<DeployReport, PortError> {
            unreachable!("wait_healthy never deploys")
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            Ok(self.healthy)
        }
    }

    /// An endpoint that never comes up must resolve to a failed check once the
    /// bound elapses — not hang, and not give up after a single probe.
    #[tokio::test(start_paused = true)]
    async fn never_healthy_resolves_false_within_the_bound() {
        let deploy = FakeDeploy {
            healthy: false,
            probes: AtomicUsize::new(0),
        };
        let started = tokio::time::Instant::now();

        let result = deploy.wait_healthy(8101, Duration::from_secs(9)).await;

        assert!(
            !result.passed,
            "a never-healthy endpoint must fail the gate"
        );
        assert!(
            started.elapsed() <= Duration::from_secs(9),
            "the gate must not overrun its bound; took {:?}",
            started.elapsed()
        );
        assert!(
            deploy.probes.load(Ordering::SeqCst) > 1,
            "the gate must poll across the bound, not probe once and quit"
        );
    }

    /// A healthy endpoint costs exactly one probe and no waiting — the gate is
    /// mandatory, but it must not slow down deploys that are fine.
    #[tokio::test(start_paused = true)]
    async fn already_healthy_resolves_immediately_on_the_first_probe() {
        let deploy = FakeDeploy {
            healthy: true,
            probes: AtomicUsize::new(0),
        };
        let started = tokio::time::Instant::now();

        let result = deploy.wait_healthy(8101, Duration::from_secs(60)).await;

        assert!(result.passed, "a healthy endpoint must pass the gate");
        assert_eq!(
            deploy.probes.load(Ordering::SeqCst),
            1,
            "a healthy endpoint must not be re-polled"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a healthy endpoint must not cost a poll interval; took {:?}",
            started.elapsed()
        );
    }

    /// The default `health_check` has no HTTP status to report (it wraps a TCP
    /// liveness bool) but must still carry timing, so deploy history records
    /// something for adapters that haven't overridden it.
    #[tokio::test(start_paused = true)]
    async fn default_health_check_reports_timing_without_a_status() {
        let deploy = FakeDeploy {
            healthy: true,
            probes: AtomicUsize::new(0),
        };

        let result = deploy.health_check(8101).await;

        assert!(result.passed);
        assert_eq!(result.http_status, None);
        assert!(result.response_time_ms.is_some());
    }

    // --- COX-B009: the shared gate every deploy call site runs through -----

    /// An adapter whose probe cannot run at all.
    struct ErroringDeploy;
    #[async_trait::async_trait]
    impl DeployPort for ErroringDeploy {
        async fn deploy(&self, _work_dir: &Path) -> Result<DeployReport, PortError> {
            unreachable!("the gate never deploys")
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            Err(PortError::Backend("probe could not run".to_owned()))
        }
    }

    /// A probe that errors is not evidence the app bound its port — it must
    /// fail the gate, not wave the deploy through. Otherwise a broken health
    /// check is itself a way to skip a gate the team requires be unskippable.
    #[tokio::test(start_paused = true)]
    async fn a_probe_that_errors_fails_the_gate() {
        let deploy: Arc<dyn DeployPort> = Arc::new(ErroringDeploy);

        assert!(
            !super::verify_deploy_health(&deploy, Some(8101)).await,
            "an erroring health probe must fail the shared deploy gate"
        );
    }

    /// The COX-B004 scenario the gate exists for: containers start, the app
    /// inside never binds its port.
    #[tokio::test(start_paused = true)]
    async fn a_port_that_never_binds_fails_the_gate() {
        let deploy: Arc<dyn DeployPort> = Arc::new(FakeDeploy {
            healthy: false,
            probes: AtomicUsize::new(0),
        });

        assert!(
            !super::verify_deploy_health(&deploy, Some(8101)).await,
            "a port that never accepts connections must fail the shared gate"
        );
    }

    /// No configured `host_port` means there is nothing to probe — the gate
    /// passes rather than failing every deploy of a port-less project.
    #[tokio::test(start_paused = true)]
    async fn no_configured_host_port_passes_without_probing() {
        let probing = Arc::new(FakeDeploy {
            healthy: false,
            probes: AtomicUsize::new(0),
        });
        let deploy: Arc<dyn DeployPort> = Arc::clone(&probing) as Arc<dyn DeployPort>;

        assert!(
            super::verify_deploy_health(&deploy, None).await,
            "a project with no published host_port has nothing to probe"
        );
        assert_eq!(
            probing.probes.load(Ordering::SeqCst),
            0,
            "with no port configured the gate must not probe at all"
        );
    }

    /// A healthy app passes the gate on the first probe — the gate is
    /// mandatory, but must not delay deploys that are fine.
    #[tokio::test(start_paused = true)]
    async fn a_healthy_app_passes_the_gate_on_the_first_probe() {
        let probing = Arc::new(FakeDeploy {
            healthy: true,
            probes: AtomicUsize::new(0),
        });
        let deploy: Arc<dyn DeployPort> = Arc::clone(&probing) as Arc<dyn DeployPort>;

        assert!(super::verify_deploy_health(&deploy, Some(8101)).await);
        assert_eq!(
            probing.probes.load(Ordering::SeqCst),
            1,
            "a healthy app must not be re-polled"
        );
    }
}
