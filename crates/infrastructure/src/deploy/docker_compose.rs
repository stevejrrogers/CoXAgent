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

async fn running_services(work_dir: &Path) -> Vec<String> {
    let Ok(out) = Command::new("docker")
        .args(["compose", "ps", "--services", "--status", "running"])
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
    async fn run_tests(&self, work_dir: &Path) -> Result<DeployReport, PortError> {
        let Some((cmd, args)) = test_command(work_dir) else {
            return Ok(DeployReport {
                success: true,
                deployed: false,
                summary: "no recognised test runner".to_owned(),
            });
        };
        let output = tokio::time::timeout(
            Duration::from_secs(900),
            Command::new(cmd)
                .args(&args)
                .current_dir(work_dir)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| PortError::Backend("test run timed out".to_owned()))?
        .map_err(|e| PortError::Backend(format!("spawn {cmd}: {e}")))?;
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
        let _ = Command::new("docker")
            .args(["compose", "down", "--remove-orphans"])
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
        let _ = Command::new("docker")
            .args(["compose", "down", "--remove-orphans"])
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await;

        let mut cmd = Command::new("docker");
        cmd.arg("compose")
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
                .args(["compose", "up", "-d", "--build"])
                .current_dir(work_dir)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            output = tokio::time::timeout(DEPLOY_TIMEOUT, retry.output())
                .await
                .map_err(|_| PortError::Backend("docker compose timed out".to_owned()))?
                .map_err(|e| PortError::Backend(format!("spawn docker: {e}")))?;
        }

        let success = output.status.success();
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
