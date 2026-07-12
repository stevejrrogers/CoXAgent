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

/// Deploys via the `docker` CLI.
#[derive(Default)]
pub struct DockerComposeDeploy;

impl DockerComposeDeploy {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DeployPort for DockerComposeDeploy {
    async fn deploy(&self, work_dir: &Path) -> Result<DeployReport, PortError> {
        if !COMPOSE_FILES.iter().any(|f| work_dir.join(f).exists()) {
            return Ok(DeployReport {
                success: true,
                deployed: false,
                summary: "no compose file — deploy skipped".to_owned(),
            });
        }

        let mut cmd = Command::new("docker");
        cmd.arg("compose")
            .arg("up")
            .arg("-d")
            .arg("--build")
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);

        let output = tokio::time::timeout(DEPLOY_TIMEOUT, cmd.output())
            .await
            .map_err(|_| PortError::Backend("docker compose timed out".to_owned()))?
            .map_err(|e| PortError::Backend(format!("spawn docker: {e}")))?;

        let success = output.status.success();
        let summary = if success {
            "docker compose up -d --build succeeded".to_owned()
        } else {
            let err = String::from_utf8_lossy(&output.stderr);
            format!(
                "docker compose failed: {}",
                err.lines().last().unwrap_or("").trim()
            )
        };
        Ok(DeployReport {
            success,
            deployed: true,
            summary,
        })
    }
}
