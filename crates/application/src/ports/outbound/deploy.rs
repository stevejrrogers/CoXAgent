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

/// Lint gate measurement: the error count, a bounded sample of the actual
/// error lines so repair prompts can name the offending lints, and the source
/// files they point at so a regression can be attributed to the change that
/// caused it rather than to whoever happens to be holding the ticket.
#[derive(Debug, Clone, Default)]
pub struct LintReport {
    /// Number of lint errors in the workspace.
    pub errors: u64,
    /// Up to a few of the raw error lines (may be empty when unsupported).
    pub sample: String,
    /// Repo-relative paths named by the errors, in report order. Empty when the
    /// linter's output carries no locations.
    pub files: Vec<String>,
}

/// Result of verifying that the code still compiles for a platform this host
/// is not. `available: false` is not a pass — it means the check could not run,
/// and saying so out loud is the whole point: a blind spot nobody reports is
/// how the same Linux-only build error gets filed three times.
#[derive(Debug, Clone, Default)]
pub struct CrossCheck {
    /// Whether the toolchain could actually perform the check.
    pub available: bool,
    /// Why it could not, when it could not (shown to humans and agents).
    pub reason: String,
    /// Compiler error lines for the foreign target, empty when it compiles.
    pub errors: Vec<String>,
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
            ..LintReport::default()
        }))
    }

    /// Compile the workspace for the deploy platform (Linux) without running
    /// it, so a platform-gated symbol that is dead code there is caught on the
    /// machine that wrote it rather than in a Docker build nobody watches.
    /// Default: unavailable, with a reason.
    ///
    /// # Errors
    /// [`PortError`] only when the check itself cannot be attempted.
    async fn cross_target_check(&self, _work_dir: &Path) -> Result<CrossCheck, PortError> {
        Ok(CrossCheck {
            available: false,
            reason: "no cross-target check for this project type".to_owned(),
            errors: Vec::new(),
        })
    }

    /// Run only the tests that could be affected by `changed` (paths relative
    /// to `work_dir`). A per-ticket gate does not need the whole suite: on
    /// this workspace the full run costs 7-8 minutes and the scoped one
    /// seconds, and the full suite still runs at the sprint boundary and on
    /// the merged tree. Default: fall back to everything, so an adapter that
    /// cannot scope stays correct.
    ///
    /// # Errors
    /// [`PortError`] when the runner cannot be started or times out.
    async fn run_tests_scoped(
        &self,
        work_dir: &Path,
        changed: &[String],
    ) -> Result<DeployReport, PortError> {
        let _ = changed;
        self.run_tests(work_dir).await
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
/// this gate existed. Port `0` is neither of those: it is in `u16` range but
/// nothing can ever connect to it, so it fails the gate at once with a config
/// diagnostic instead of being polled to exhaustion (COX-B042).
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
    // `Some(0)` is corrupt config, not a dead app: probing it would spend the
    // whole polling window and then report "the app never bound its port",
    // which is indistinguishable from a real app bug. Fail immediately and
    // name the actual cause. Configs read from disk are healed at load time
    // (`heal_host_port`); this guards every other way a `Config` is built.
    if port == 0 {
        tracing::error!(
            "deploy.host_port is 0, which is not a connectable TCP port — failing the \
             deploy health gate on the config, not on the app; set a real port in \
             coxagent.json"
        );
        return false;
    }
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

/// Parse `deploy.host_port` out of a project's raw `coxagent.json`,
/// independent of whether the rest of the file parses as a valid [`Config`]
/// (COX-B035: `#[serde(default)]` on `DeployConfig::host_port` only rescues
/// an absent key, not a type/range mismatch — so once `Config` deserialization
/// has failed and a caller has fallen back to `Config::default()`, the
/// distinction between "no host_port configured" and "host_port present but
/// corrupt" is already lost). A missing/unreadable file, unparseable JSON, a
/// missing `host_port` key, or an explicit `null` all mean "nothing
/// configured" — same contract as `Option<u16>` and `verify_deploy_health`'s
/// no-port pass. Any other JSON value that isn't a valid non-negative `u16`
/// (negative, float, string, bool, out of range) is a corrupt config and must
/// fail the gate rather than being folded into "nothing configured" —
/// `serde_json::Value::as_u64` returns `None` for all of those just as it
/// does for a genuinely absent field, so the raw JSON value must be inspected
/// instead of going through `as_u64` first. `0` parses as an in-range `u16`
/// but is not a connectable TCP port (COX-B042); rejecting it here means the
/// gate fails once, right away, instead of `wait_healthy` polling a port that
/// can never accept a connection until its own timeout gives up.
///
/// [`Config`]: crate::config::Config
pub fn parse_deploy_host_port(raw_config: &str) -> Result<Option<u16>, ()> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_config) else {
        return Ok(None);
    };
    match value.get("deploy").and_then(|d| d.get("host_port")) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => match v.as_u64().and_then(|n| u16::try_from(n).ok()) {
            Some(0) | None => Err(()),
            Some(port) => Ok(Some(port)),
        },
    }
}

/// Run [`verify_deploy_health`] against a probe port that may itself be
/// invalid (COX-B025/COX-B026/COX-B035): every call site that reads
/// `deploy.host_port` from raw config text via [`parse_deploy_host_port`]
/// (rather than trusting `Config::deploy.host_port`, which cannot tell
/// "absent" from "malformed" once deserialization has already defaulted it
/// away) runs the result through this so a corrupt port fails the gate
/// outright instead of being treated as "nothing configured" — which would
/// pass unconditionally and report a dead app as healthy.
pub async fn verify_deploy_health_probe(
    deploy: &Arc<dyn DeployPort>,
    probe: Result<Option<u16>, ()>,
) -> bool {
    match probe {
        Ok(port) => verify_deploy_health(deploy, port).await,
        Err(()) => false,
    }
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

    // --- COX-B042: `host_port: 0` is not a valid TCP port ------------------

    /// `0` deserializes fine as a `u16` — it is in range — but it is not a
    /// port anything can ever connect to. It must be treated the same as any
    /// other corrupt `host_port` (negative/float/string), not folded into
    /// "nothing configured", or `verify_deploy_health_probe` would probe a
    /// port that can never bind and fail every deploy forever instead of
    /// rejecting the config outright.
    #[test]
    fn a_zero_host_port_is_rejected_rather_than_treated_as_unconfigured() {
        assert_eq!(
            super::parse_deploy_host_port(r#"{"deploy":{"host_port":0}}"#),
            Err(()),
            "host_port 0 must be rejected as corrupt config, not accepted as a real port"
        );
    }

    /// The gate itself, reached by any caller whose `Config` holds `Some(0)`
    /// without passing through `parse_deploy_host_port` (the cycle's default
    /// `host_port_probe` is `Ok(config.deploy.host_port)`). Port 0 can never
    /// accept a connection, so probing it burns the whole polling window and
    /// then blames the app. It must fail at once, without probing.
    #[tokio::test(start_paused = true)]
    async fn a_zero_host_port_fails_the_gate_at_once_instead_of_being_probed() {
        let probing = Arc::new(FakeDeploy {
            healthy: true, // even a would-be-healthy adapter must not be asked
            probes: AtomicUsize::new(0),
        });
        let deploy: Arc<dyn DeployPort> = Arc::clone(&probing) as Arc<dyn DeployPort>;
        let started = tokio::time::Instant::now();

        assert!(
            !super::verify_deploy_health(&deploy, Some(0)).await,
            "host_port 0 is corrupt config — the gate must fail, not pass vacuously"
        );
        assert_eq!(
            probing.probes.load(Ordering::SeqCst),
            0,
            "a port nothing can bind must not be probed at all"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the gate must reject 0 immediately, not poll it to exhaustion; took {:?}",
            started.elapsed()
        );
    }
}
