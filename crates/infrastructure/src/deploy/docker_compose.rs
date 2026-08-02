//! `DockerComposeDeploy` — deploys a codebase with `docker compose up -d
//! --build`. Skips gracefully (success, not deployed) when the project has no
//! compose file, so non-dockerised projects don't error the cycle.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{DeployPort, DeployReport};
use coxagent_application::PortError;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const COMPOSE_FILES: &[&str] = &[
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(900);

/// Names of currently-running compose services (best-effort; empty on error).
/// Pull the host port out of a compose bind error like
/// `Bind for 0.0.0.0:8100 failed: port is already allocated`.
fn extract_bind_port(err: &str) -> Option<String> {
    let idx = err.find("Bind for ")?;
    let rest = &err[idx + "Bind for ".len()..];
    let addr = rest.split_whitespace().next()?;
    Some(addr.rsplit(':').next()?.trim().to_owned())
}

/// The compose project name of whatever container currently publishes `port`
/// on this host, or `None` when the squatter isn't compose-managed (we never
/// evict arbitrary containers).
async fn compose_project_on_port(port: &str) -> Option<String> {
    let out = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("publish={port}"),
            "--format",
            "{{.Label \"com.docker.compose.project\"}}",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// Clamp every container of this compose project to a CPU/memory budget via
/// `docker update`, regardless of what the agent-authored compose file says —
/// a runaway service (busy loop, leak) can then never take the whole host.
/// Defaults: 2 CPUs, 1g memory. Override with `COXAGENT_DEPLOY_CPUS` /
/// `COXAGENT_DEPLOY_MEM`; set either to `off` to skip. Best-effort.
async fn apply_resource_limits(proj: &str) {
    let cpus = std::env::var("COXAGENT_DEPLOY_CPUS").unwrap_or_else(|_| "2".to_owned());
    let mem = std::env::var("COXAGENT_DEPLOY_MEM").unwrap_or_else(|_| "1g".to_owned());
    if cpus.eq_ignore_ascii_case("off") || mem.eq_ignore_ascii_case("off") {
        return;
    }
    let Ok(out) = Command::new("docker")
        .args(["compose", "-p", proj, "ps", "-q"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return;
    };
    for id in String::from_utf8_lossy(&out.stdout).lines() {
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let _ = Command::new("docker")
            .args([
                "update",
                "--cpus",
                &cpus,
                "--memory",
                &mem,
                "--memory-swap",
                &mem,
                id,
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .await;
    }
}

/// The id of ANY container publishing `port` (compose-labelled or not).
async fn container_on_port(port: &str) -> Option<String> {
    let out = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("publish={port}"),
            "--format",
            "{{.ID}}",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    (!id.is_empty()).then_some(id)
}

/// Deterministic compose project name for a deploy dir: `cox-<parent>-<dir>`
/// (sanitized). Ends the era of accidental project names like "116" or
/// "codebase" colliding/littering docker — every CoXAgent deploy is grouped
/// and identifiable, and the janitor can target the `cox-` prefix safely.
fn compose_project_name(work_dir: &Path) -> String {
    let comp = |o: Option<&std::ffi::OsStr>| {
        o.map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
    };
    let dir = comp(work_dir.file_name());
    let parent = comp(work_dir.parent().and_then(|p| p.file_name()));
    let mut name = format!("cox-{parent}-{dir}");
    name.truncate(60);
    name.trim_matches('-').to_owned()
}

async fn running_services(work_dir: &Path) -> Vec<String> {
    let proj = compose_project_name(work_dir);
    let Ok(out) = Command::new("docker")
        .args([
            "compose",
            "-p",
            &proj,
            "ps",
            "--services",
            "--status",
            "running",
        ])
        .current_dir(work_dir)
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Deploys via the `docker` CLI.
#[derive(Default)]
pub struct DockerComposeDeploy;

impl DockerComposeDeploy {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Whether the Docker daemon answers `docker info`.
async fn daemon_up() -> bool {
    Command::new("docker")
        .args(["info"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Detect the project's toolchain and return its test command, or `None`.
fn test_command(work_dir: &Path) -> Option<(&'static str, Vec<&'static str>)> {
    let has = |f: &str| work_dir.join(f).exists();
    if has("Cargo.toml") {
        Some(("cargo", vec!["test", "--quiet"]))
    } else if has("go.mod") {
        Some(("go", vec!["test", "./..."]))
    } else if has("package.json") {
        Some(("npm", vec!["test", "--silent"]))
    } else if has("pyproject.toml") || has("pytest.ini") || has("requirements.txt") {
        Some(("pytest", vec!["-q"]))
    } else {
        None
    }
}

#[async_trait]
impl DeployPort for DockerComposeDeploy {
    async fn lint(&self, work_dir: &Path) -> Result<Option<u64>, PortError> {
        // Rust-only for now: clippy's error count is the lint currency the
        // DoD gate compares against the project baseline.
        if !work_dir.join("Cargo.toml").exists() {
            return Ok(None);
        }
        let _slot = crate::proc::heavy_slot().await;
        let child = crate::proc::low_priority("cargo")
            .args(["clippy", "--workspace", "--all-targets", "--quiet"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn clippy: {e}")))?;
        let leader = child.id();
        let Ok(out) =
            tokio::time::timeout(Duration::from_secs(600), child.wait_with_output()).await
        else {
            if let Some(pid) = leader {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend("clippy timed out".to_owned()));
        };
        let out = out.map_err(|e| PortError::Backend(format!("clippy wait: {e}")))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let count = text
            .lines()
            .filter(|l| l.trim_start().starts_with("error"))
            .count() as u64;
        Ok(Some(count))
    }

    async fn lint_report(
        &self,
        work_dir: &Path,
    ) -> Result<Option<coxagent_application::ports::outbound::LintReport>, PortError> {
        if !work_dir.join("Cargo.toml").exists() {
            return Ok(None);
        }
        let _slot = crate::proc::heavy_slot().await;
        let child = crate::proc::low_priority("cargo")
            .args(["clippy", "--workspace", "--all-targets", "--quiet"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn clippy: {e}")))?;
        let leader = child.id();
        let Ok(out) =
            tokio::time::timeout(Duration::from_secs(600), child.wait_with_output()).await
        else {
            if let Some(pid) = leader {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend("clippy timed out".to_owned()));
        };
        let out = out.map_err(|e| PortError::Backend(format!("clippy wait: {e}")))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let (errors, files) = parse_clippy(&text);
        // A dozen error lines is plenty for a repair prompt; the agent can run
        // clippy itself for the rest.
        let sample = errors
            .iter()
            .take(12)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Some(coxagent_application::ports::outbound::LintReport {
            errors: errors.len() as u64,
            sample,
            files,
        }))
    }

    async fn cross_target_check(
        &self,
        work_dir: &Path,
    ) -> Result<coxagent_application::ports::outbound::CrossCheck, PortError> {
        use coxagent_application::ports::outbound::CrossCheck;
        const TARGET: &str = "x86_64-unknown-linux-gnu";
        if !work_dir.join("Cargo.toml").exists() {
            return Ok(CrossCheck {
                available: false,
                reason: "not a cargo project".to_owned(),
                errors: Vec::new(),
            });
        }
        // Self-provision rather than wait to be told. A capability the check
        // needs and can install itself is not a reason to skip verifying — that
        // silence is exactly how the same Linux-only dead_code error got filed
        // three times while every gate on this macOS host stayed green.
        let mut installed = linux_target_installed(TARGET).await;
        if !installed {
            let added = Command::new("rustup")
                .args(["target", "add", TARGET])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .is_ok_and(|s| s.success());
            if added {
                installed = linux_target_installed(TARGET).await;
                if installed {
                    tracing::info!("cross-check installed the {TARGET} target itself");
                }
            }
        }
        if !installed {
            // No rustup to extend — but Docker compiles for Linux by
            // definition, and it is the build that actually breaks. Use it.
            if let Some(check) = compose_build_check(work_dir).await {
                return Ok(check);
            }
            return Ok(CrossCheck {
                available: false,
                reason: format!(
                    "cannot verify the {TARGET} build: `rustup target add {TARGET}` did not \
                     succeed (rustup may be absent) and no Docker build is available here. Until \
                     one of them exists, a symbol that is dead code on Linux compiles clean on \
                     this host and only breaks in Docker/CI."
                ),
                errors: Vec::new(),
            });
        }
        let _slot = crate::proc::heavy_slot().await;
        let child = crate::proc::low_priority("cargo")
            .args(["check", "--workspace", "--all-targets", "--target", TARGET])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn cargo check: {e}")))?;
        let leader = child.id();
        let Ok(out) =
            tokio::time::timeout(Duration::from_secs(900), child.wait_with_output()).await
        else {
            if let Some(pid) = leader {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend(
                "cross-target check timed out".to_owned(),
            ));
        };
        let out = out.map_err(|e| PortError::Backend(format!("cargo check wait: {e}")))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let (errors, _files) = parse_clippy(&text);
        Ok(CrossCheck {
            available: true,
            reason: String::new(),
            errors: errors.into_iter().take(12).collect(),
        })
    }

    async fn run_tests(&self, work_dir: &Path) -> Result<DeployReport, PortError> {
        let Some((cmd, args)) = test_command(work_dir) else {
            return Ok(DeployReport {
                success: true,
                deployed: false,
                summary: "no recognised test runner".to_owned(),
            });
        };
        // Host-wide gate + nice: at most COXAGENT_MAX_PARALLEL_HEAVY test
        // suites run at once across ALL projects, and each runs at background
        // priority — N projects can no longer freeze the machine together.
        let _slot = crate::proc::heavy_slot().await;
        let child = crate::proc::low_priority(cmd)
            .args(&args)
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn {cmd}: {e}")))?;
        let leader = child.id();
        let Ok(output) =
            // 30 minutes: a cold target dir (fresh agent branch) compiles the
            // whole workspace before a single test runs — 15 was not enough.
            tokio::time::timeout(Duration::from_secs(1800), child.wait_with_output()).await
        else {
            // Kill the whole test-runner tree, not just `nice`.
            if let Some(pid) = leader {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend("test run timed out".to_owned()));
        };
        let output = output.map_err(|e| PortError::Backend(format!("{cmd} wait: {e}")))?;
        let success = output.status.success();
        let tail = |b: &[u8]| -> String {
            String::from_utf8_lossy(b)
                .lines()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        };
        let summary = if success {
            format!("{cmd} {} passed", args.join(" "))
        } else {
            let out = tail(&output.stdout);
            let err = tail(&output.stderr);
            format!("{cmd} tests failed:\n{err}\n{out}")
        };
        Ok(DeployReport {
            success,
            deployed: true,
            summary,
        })
    }

    async fn down(&self, work_dir: &Path) -> Result<(), PortError> {
        let proj = compose_project_name(work_dir);
        let _ = Command::new("docker")
            .args(["compose", "-p", &proj, "down", "--remove-orphans"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| PortError::Backend(format!("compose down: {e}")))?;
        Ok(())
    }

    async fn health(&self, port: u16) -> Result<bool, PortError> {
        // Something accepting TCP on the published port = the app is up.
        let addr = format!("127.0.0.1:{port}");
        let ok = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .is_ok_and(|r| r.is_ok());
        Ok(ok)
    }

    /// COX-F005: an HTTP GET against the app's root, unlike [`Self::health`]
    /// this captures the HTTP status and response time so a deploy attempt's
    /// health outcome can be recorded in full, not just as a bool. Bounded by
    /// a fixed per-probe timeout — connection refused, DNS failure, and a
    /// wedged connection are all reported as `passed: false`, never left
    /// hanging.
    async fn health_check(&self, port: u16) -> coxagent_application::state::HealthCheckResult {
        let url = format!("http://127.0.0.1:{port}/");
        let start = std::time::Instant::now();
        let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        else {
            return coxagent_application::state::HealthCheckResult {
                passed: false,
                http_status: None,
                response_time_ms: None,
            };
        };
        match client.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status();
                // Any answer that isn't a 5xx means the app bound the port and
                // is serving — which is exactly what this gate exists to prove
                // (COX-B001: compose exits 0 while the app inside never
                // listens). Deployed projects are arbitrary, so demanding a 2xx
                // at `/` would roll back every app that simply has no root
                // route; a 5xx, by contrast, is a genuinely broken app.
                coxagent_application::state::HealthCheckResult {
                    passed: !status.is_server_error(),
                    http_status: Some(status.as_u16()),
                    response_time_ms: Some(
                        u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    ),
                }
            }
            Err(_) => coxagent_application::state::HealthCheckResult {
                passed: false,
                http_status: None,
                response_time_ms: Some(
                    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                ),
            },
        }
    }

    async fn ensure_daemon(&self) -> Result<bool, PortError> {
        if daemon_up().await {
            return Ok(true);
        }
        // Try to start it: Docker Desktop on macOS, systemd on Linux.
        #[cfg(target_os = "macos")]
        let _ = Command::new("open")
            .args(["-a", "Docker"])
            .stdin(std::process::Stdio::null())
            .status()
            .await;
        #[cfg(target_os = "linux")]
        let _ = Command::new("systemctl")
            .args(["start", "docker"])
            .stdin(std::process::Stdio::null())
            .status()
            .await;
        // Poll for it to come up (Docker Desktop can take a while to boot).
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if daemon_up().await {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // Linear up → evict-retry → summarize pass; splitting it would obscure it.
    #[allow(clippy::too_many_lines)]
    async fn deploy(&self, work_dir: &Path) -> Result<DeployReport, PortError> {
        if !COMPOSE_FILES.iter().any(|f| work_dir.join(f).exists()) {
            return Ok(DeployReport {
                success: true,
                deployed: false,
                summary: "no compose file — deploy skipped".to_owned(),
            });
        }

        // Bring the previous stack down first (best-effort). Without this, a
        // still-running container from a prior cycle keeps holding its host
        // ports, so `up` fails with "port is already allocated" — a recurring,
        // self-inflicted deploy blocker. `down --remove-orphans` releases the
        // project's own ports (and orphaned services) so `up` starts clean.
        let proj = compose_project_name(work_dir);
        let _ = Command::new("docker")
            .args(["compose", "-p", &proj, "down", "--remove-orphans"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await;

        // Compose builds are as heavy as test suites — same host-wide gate.
        let _slot = crate::proc::heavy_slot().await;
        let mut cmd = Command::new("docker");
        cmd.arg("compose")
            .args(["-p", &proj])
            .arg("up")
            .arg("-d")
            .arg("--build")
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut output = tokio::time::timeout(DEPLOY_TIMEOUT, cmd.output())
            .await
            .map_err(|_| PortError::Backend("docker compose timed out".to_owned()))?
            .map_err(|e| PortError::Backend(format!("spawn docker: {e}")))?;

        // Self-heal a squatted port: if `up` failed because the host port is
        // held by ANOTHER compose project on this machine (typically a stale
        // PR preview or an old build with a different project name), evict that
        // project and retry once — instead of failing every cycle until a human
        // notices.
        let mut evicted = None;
        // Up to two eviction+retry rounds: round 1 handles a stale compose
        // project; round 2 (or when no compose label exists) stops whatever
        // raw container is squatting the port. Docker also needs a beat to
        // release a freshly-stopped binding, hence the short sleep.
        for round in 0..2u8 {
            if output.status.success() {
                break;
            }
            let err = String::from_utf8_lossy(&output.stderr).to_string();
            if !err.contains("port is already allocated") {
                break;
            }
            let Some(port) = extract_bind_port(&err) else {
                break;
            };
            if let Some(project) = compose_project_on_port(&port).await {
                let _ = Command::new("docker")
                    .args(["compose", "-p", &project, "down", "--remove-orphans"])
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output()
                    .await;
                evicted = Some(format!("compose project `{project}`"));
            } else if let Some(id) = container_on_port(&port).await {
                let _ = Command::new("docker")
                    .args(["stop", &id])
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output()
                    .await;
                evicted = Some(format!("container `{id}`"));
            } else if round > 0 {
                break; // nothing visible holds the port — give up, report
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let mut retry = Command::new("docker");
            retry
                .args(["compose", "-p", &proj, "up", "-d", "--build"])
                .current_dir(work_dir)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            output = tokio::time::timeout(DEPLOY_TIMEOUT, retry.output())
                .await
                .map_err(|_| PortError::Backend("docker compose timed out".to_owned()))?
                .map_err(|e| PortError::Backend(format!("spawn docker: {e}")))?;
        }

        let success = output.status.success();
        if success {
            apply_resource_limits(&proj).await;
        }
        let summary = if success {
            let note = evicted
                .map(|p| format!(" (evicted stale {p} off the port)"))
                .unwrap_or_default();
            match running_services(work_dir).await {
                services if !services.is_empty() => {
                    format!("running: {}{note}", services.join(", "))
                }
                _ => format!("docker compose up -d --build succeeded{note}"),
            }
        } else {
            // Compose prints the real cause somewhere in stderr, but the final
            // line is often blank; surface the last *non-empty* line (falling
            // back to stdout, then the exit code) so the reason is never empty.
            let err = String::from_utf8_lossy(&output.stderr);
            let out = String::from_utf8_lossy(&output.stdout);
            let last_meaningful = |s: &str| -> Option<String> {
                s.lines()
                    .map(str::trim)
                    .rev()
                    .find(|l| !l.is_empty())
                    .map(ToOwned::to_owned)
            };
            let detail = last_meaningful(&err)
                .or_else(|| last_meaningful(&out))
                .unwrap_or_else(|| {
                    format!("exit {} (no output)", output.status.code().unwrap_or(-1))
                });
            format!("docker compose failed: {detail}")
        };
        Ok(DeployReport {
            success,
            deployed: true,
            summary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC (COX-F005): a health endpoint that's unreachable (nothing
    /// listening — connection refused) must be treated as a failed check,
    /// bounded by a timeout, never left hanging indefinitely.
    #[tokio::test]
    async fn unreachable_health_endpoint_is_a_bounded_failure_not_a_hang() {
        // No listener is ever bound to this port by this test.
        let unreachable_port = 65_533;

        let outcome = tokio::time::timeout(
            Duration::from_secs(35),
            DockerComposeDeploy.health_check(unreachable_port),
        )
        .await
        .expect(
            "a connection-refused health endpoint must not hang past the ~30s bound \
             (COX-F005)",
        );

        assert!(
            !outcome.passed,
            "connection refused must be reported as a failed health check, not a pass: \
             {outcome:?}"
        );
    }

    /// Regression test (COX-B018): the mandatory gate must poll to the
    /// configured timeout, not stop at one probe. `health_check` itself is a
    /// single bounded probe (a fixed 5s reqwest timeout) by design — it's
    /// `wait_healthy` (the trait's default) that turns it into a poll loop.
    /// This exercises the real adapter (not a scripted double): nothing is
    /// listening on the port for the first 8s (connection refused, same as a
    /// container whose app hasn't bound its port yet), then a real TCP
    /// listener comes up and answers 200 OK — the kind of cold-start delay a
    /// container doing DB migrations can have. 8s is longer than a single
    /// probe cycle but well inside the 20s bound below.
    #[tokio::test]
    async fn slow_starting_app_within_the_bound_passes_via_the_poll_loop() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // Reserve a port, then release it so nothing answers on it yet.
        let probe = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(8)).await;
            let listener = TcpListener::bind(("127.0.0.1", port))
                .await
                .expect("rebind once the app 'finishes starting'");
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                        .await;
                });
            }
        });

        let result = DockerComposeDeploy
            .wait_healthy(port, Duration::from_secs(20))
            .await;

        assert!(
            result.passed,
            "an app that binds its port within the configured timeout must pass \
             the gate, even though earlier probes hit connection refused: {result:?}"
        );
    }
}

/// Whether rustup reports `target` as installed.
async fn linux_target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains(target))
}

/// Fallback cross-check: build the project's Docker image. It compiles for
/// Linux by definition, so it catches the same class of platform breakage
/// without a cross toolchain — and it is the build that actually fails in
/// production. `None` when there is no compose file or no daemon to run it.
async fn compose_build_check(
    work_dir: &Path,
) -> Option<coxagent_application::ports::outbound::CrossCheck> {
    use coxagent_application::ports::outbound::CrossCheck;
    if !COMPOSE_FILES.iter().any(|f| work_dir.join(f).exists()) || !daemon_up().await {
        return None;
    }
    let _slot = crate::proc::heavy_slot().await;
    let proj = compose_project_name(work_dir);
    let out = tokio::time::timeout(
        DEPLOY_TIMEOUT,
        Command::new("docker")
            .args(["compose", "-p", &proj, "build"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if out.status.success() {
        return Some(CrossCheck {
            available: true,
            reason: String::new(),
            errors: Vec::new(),
        });
    }
    let text = String::from_utf8_lossy(&out.stderr);
    let (errors, _) = parse_clippy(&text);
    Some(CrossCheck {
        available: true,
        reason: "verified via the Docker (Linux) image build".to_owned(),
        errors: if errors.is_empty() {
            vec![text.lines().rev().take(3).collect::<Vec<_>>().join(" | ")]
        } else {
            errors.into_iter().take(12).collect()
        },
    })
}

/// Split `cargo clippy` human output into its error lines and the files those
/// errors point at. Clippy prints the location on the `-->` line that follows
/// each error, so the two are paired in report order.
fn parse_clippy(text: &str) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut files = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.trim_start().starts_with("error") {
            continue;
        }
        errors.push(line.trim().to_owned());
        // The location follows within a couple of lines; stop at the next error
        // so an error without one never steals the following error's file.
        let mut file = String::new();
        for _ in 0..3 {
            let Some(next) = lines.peek() else { break };
            let next = (*next).trim();
            if next.starts_with("error") {
                break;
            }
            if let Some(loc) = next.strip_prefix("--> ") {
                loc.split(':')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .clone_into(&mut file);
                lines.next();
                break;
            }
            lines.next();
        }
        files.push(file);
    }
    (errors, files)
}

#[cfg(test)]
mod clippy_parse_tests {
    use super::parse_clippy;

    #[test]
    fn pairs_each_error_with_the_file_it_points_at() {
        let out = "\
error: redundant closure
  --> crates/app/src/lib.rs:12:5
   |
error: too many lines
  --> crates/domain/src/ticket.rs:99:1
   |
error: could not compile `x` due to 2 previous errors
";
        let (errors, files) = parse_clippy(out);
        assert_eq!(errors.len(), 3);
        assert_eq!(
            files,
            [
                "crates/app/src/lib.rs",
                "crates/domain/src/ticket.rs",
                // The summary line carries no location and must not borrow one.
                ""
            ]
        );
    }
}

#[cfg(test)]
mod cross_check_tests {
    use super::DockerComposeDeploy;
    use coxagent_application::ports::outbound::DeployPort;

    #[tokio::test]
    async fn a_non_cargo_project_is_reported_unavailable_with_a_reason() {
        // "Unavailable" must always carry why. A blank reason is how a blind
        // spot becomes invisible again.
        let dir = std::env::temp_dir().join(format!("crosschk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let check = DockerComposeDeploy::new()
            .cross_target_check(&dir)
            .await
            .expect("check");
        assert!(!check.available);
        assert!(
            !check.reason.trim().is_empty(),
            "reason must say what to do"
        );
        assert!(check.errors.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
