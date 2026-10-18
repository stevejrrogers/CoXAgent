//! `GET /api/projects/:pid/preflight` — the go-live readiness preflight
//! (CXA-F239): one JSON object answering "is this workspace ready to ship its
//! first Verified ticket?" before the operator burns agent cycles discovering
//! it one failed gate at a time.
//!
//! This file is the ADAPTER half: it takes ONE snapshot of the workspace
//! through the ports the hub already holds (the workspace-files port for the
//! raw `coxagent.json`, the deploy port for the docker daemon and compose
//! probes, the auth port for the account posture, the startup tooling probe
//! for the docker CLI) plus one bind probe for the publish port, then hands
//! the snapshot to the PURE decision in
//! [`coxagent_application::use_cases::readiness_preflight`]. No business
//! judgement lives here.

use super::*;
use coxagent_application::ports::outbound::deploy::COMPOSE_FILES;
use coxagent_application::use_cases::readiness_preflight::{
    run_preflight, DockerTooling, PreflightProbe, PreflightSnapshot, PublishPortProbe,
};

/// How long the preflight will wait for a deploy daemon that has to be
/// STARTED (Docker Desktop on macOS boots slowly). A GET that can hang for a
/// minute is hostile; after the bound the item reports honestly that the
/// daemon did not come up in time.
const DAEMON_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn preflight_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let snapshot = snapshot(&app, &p).await;
    Json(run_preflight(&snapshot)).into_response()
}

/// Take ONE snapshot of the workspace — every external fact gathered once,
/// here, so the decision in the application layer stays a pure function.
async fn snapshot(app: &AppState, p: &ProjectHandle) -> PreflightSnapshot {
    let config_text = config_text(p).await;
    // The publish-port probe follows the SAME parse every deploy call site
    // uses (`parse_deploy_host_port`), so the preflight can never disagree
    // with the health gate about what the config publishes.
    let publish_port = config_text
        .as_deref()
        .and_then(|text| {
            coxagent_application::ports::outbound::parse_deploy_host_port(text)
                .ok()
                .flatten()
        })
        .map(|port| PublishPortProbe {
            port,
            free: port_free(port),
        });
    let docker = docker_tooling(&app.tooling);
    let (daemon, compose) = probe_deploy(p, docker.as_ref()).await;
    PreflightSnapshot {
        config_text,
        detected_engines: app.engines.iter().map(|(name, _)| name.clone()).collect(),
        docker,
        daemon,
        compose,
        has_compose_file: has_compose_file(p).await,
        publish_port,
        auth_users: match &app.auth {
            Some(auth) => Some(auth.list_users().await.len()),
            None => None, // auth disabled — the snapshot says so; the decision warns
        },
    }
}

/// The raw `coxagent.json` text — through the workspace-files port, with the
/// same direct-read fallback `get_config` uses when a bare handle carries no
/// files adapter.
async fn config_text(p: &ProjectHandle) -> Option<String> {
    match &p.files {
        Some(files) => files.read(&p.config_path).await,
        None => std::fs::read_to_string(&p.config_path).ok(),
    }
}

/// Whether the workspace carries a compose document — the fact that decides
/// whether deploys will run compose at all (the adapter skips without one).
async fn has_compose_file(p: &ProjectHandle) -> Option<bool> {
    let files = p.files.as_ref()?;
    let mut found = false;
    for name in COMPOSE_FILES {
        if files.stat(&p.work_dir.join(name)).await.is_some() {
            found = true;
            break;
        }
    }
    Some(found)
}

/// The daemon + compose probes. Both are worth running only when there is an
/// adapter to ask AND the docker CLI is known to exist; anything else reads
/// as precisely what happened — "could not ask" (unknown) or "cannot run"
/// (unavailable) — never as a pass.
async fn probe_deploy(
    p: &ProjectHandle,
    docker: Option<&DockerTooling>,
) -> (PreflightProbe, PreflightProbe) {
    match (&p.deploy, docker) {
        (Some(deploy), Some(d)) if d.present => {
            let daemon = tokio::time::timeout(DAEMON_PROBE_TIMEOUT, deploy.ensure_daemon())
                .await
                .ok()
                .and_then(std::result::Result::ok)
                .unwrap_or(false);
            let compose = deploy.compose_available().await;
            (bool_probe(daemon), bool_probe(compose))
        }
        // An adapter but no tooling knowledge: the checks could not be run.
        (Some(_), None) => (PreflightProbe::Unknown, PreflightProbe::Unknown),
        // No deploy adapter, or a docker CLI known to be missing: compose
        // provably cannot run.
        (_, _) => (PreflightProbe::Unavailable, PreflightProbe::Unavailable),
    }
}

fn bool_probe(ok: bool) -> PreflightProbe {
    if ok {
        PreflightProbe::Available
    } else {
        PreflightProbe::Unavailable
    }
}

/// The docker entry the hub's startup tooling probe reported. `None` = the
/// hub booted without probing (an embedded/fixture start) — reported as
/// "unknown", never folded into a verdict.
fn docker_tooling(tooling: &serde_json::Value) -> Option<DockerTooling> {
    tooling
        .get("tools")?
        .as_array()?
        .iter()
        .find(|t| t.get("name").and_then(serde_json::Value::as_str) == Some("docker"))
        .map(|t| DockerTooling {
            present: t
                .get("present")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            install: t
                .get("install")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
}

/// The same bind probe `heal_host_port` uses to pick a port: can this host
/// actually publish on it right now? A held port is the collision that makes
/// `docker compose up` fail with "port is already allocated".
fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The startup tooling probe's serialized shape — one entry per tool.
    fn tooling_with(entries: &serde_json::Value) -> serde_json::Value {
        json!({ "os": "macos", "has_brew": true, "brew_install": "", "tools": entries })
    }

    #[test]
    fn the_docker_entry_is_found_with_its_install_hint() {
        let tooling = tooling_with(&json!([
            { "name": "git", "present": true, "install": "xcode-select --install" },
            { "name": "docker", "present": false, "install": "brew install docker" }
        ]));

        let docker = docker_tooling(&tooling).expect("docker entry exists");

        assert!(!docker.present);
        assert_eq!(docker.install, "brew install docker");
    }

    #[test]
    fn a_tooling_report_without_a_docker_entry_reads_as_unknown() {
        let tooling = tooling_with(&json!([{ "name": "git", "present": true, "install": "" }]));

        assert!(docker_tooling(&tooling).is_none());
    }

    /// `HubExtras.tooling` defaults to JSON `null` when the hub booted without
    /// probing — the snapshot must carry that as "unknown", not as "absent".
    #[test]
    fn a_null_tooling_report_reads_as_unknown() {
        assert!(docker_tooling(&serde_json::Value::Null).is_none());
    }

    #[test]
    fn a_docker_entry_missing_the_install_key_defaults_to_an_empty_hint() {
        let tooling = tooling_with(&json!([{ "name": "docker", "present": true }]));

        let docker = docker_tooling(&tooling).expect("docker entry exists");

        assert!(docker.present);
        assert_eq!(docker.install, "");
    }
}
