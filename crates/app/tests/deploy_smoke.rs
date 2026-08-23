//! COX-B008 regression guard, deploy half: a green `cargo build` is not a
//! running app.
//!
//! The bug report had two symptoms with one root cause — the Docker builder
//! failed to compile (`dead_code` on a macOS-only helper under `warnings =
//! "deny"`), so no image was produced and nothing ever answered on the
//! published host port. `platform_gates` guards the compile; this test guards
//! what the user actually asked for: run the exact command this repo's
//! docker-compose.yml documents and prove the hub answers on host port 8101.
//! It also covers failure modes a compile check can never see — a broken
//! entrypoint, a container that exits on boot, a wrong port mapping.
//!
//! `#[ignore]` because it builds a release image (minutes) and binds a host
//! port, which is wrong for a plain `cargo test`. Run it deliberately:
//!
//! ```text
//! cargo test -p coxagent-app --test deploy_smoke -- --ignored --nocapture
//! ```
//!
//! CI runs exactly that in the `deploy-smoke` job.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// This project's assigned deploy port — fixed so it never collides with a live
/// hub bound to 4000 on the same docker host (CXA-B069 made every published port
/// env-driven). The smoke test must probe whatever port *this* invocation of
/// `docker compose` actually binds, not a constant that drifts from reality when
/// `APP_PORT` leaks in from the environment (CXA-B077).
const DEFAULT_HOST_PORT: u16 = 8101;

/// How long the stack gets to build and answer before the test gives up.
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(3);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Pure decision over the environment's APP_PORT value — no IO, so it is
/// trivially unit-testable below without mutating process globals.
fn resolve_host_port(app_port: Option<&str>) -> u16 {
    match app_port {
        None | Some("") => DEFAULT_HOST_PORT,
        Some(n) => n.parse().unwrap_or_else(|why| {
            panic!("APP_PORT=`{n}` is not a valid host port ({why})")
        }),
    }
}

/// Resolve a compose host-port field into the number Docker actually binds,
/// honouring an env override. docker-compose.yml publishes `"${APP_PORT:-8101}:4000"`
/// — Docker binds `$APP_PORT` when it is set and falls back to `8101` otherwise.
///
/// Mirroring the semantics in docs_ports.rs keeps this test reading reality:
/// when a concurrent worktree holds 8101 and a redeploy passes `APP_PORT=8110`,
/// this returns 8110 so we probe exactly what *we* orchestrated instead of a
/// stale/unrelated container squatting on the default (the CXA-B077 fragility).
fn effective_host_port() -> u16 {
    resolve_host_port(std::env::var("APP_PORT").ok().as_deref())
}

fn compose(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .arg("compose")
        .args(args)
        .current_dir(root)
        // The documented bring-up command sets both required secrets inline
        // (PG_PASSWORD for Postgres, COXAGENT_ADMIN_PASSWORD for first-run
        // super-admin bootstrap); without either, compose fails interpolation.
        //
        // APP_PORT is deliberately *not* forced here: its absence lets us resolve
        // exactly what this run will bind and prove we probe it. Set it in env only
        // if you need this worktree on a non-default host port alongside another one.
        .env("PG_PASSWORD", "ci-smoke")
        .env("COXAGENT_ADMIN_PASSWORD", "ci-smoke")
        .output()
        .unwrap_or_else(|e| panic!("`docker compose {}` failed to spawn: {e}", args.join(" ")))
}

/// Whether an automated pass may tear down a docker compose project to free its
/// host port.
///
/// Mirrors production deploy's shared policy (`reclaimable::reclaimable_compose_project`)
/// rather than importing it: only agent-managed preview deployments (`cox-...`)
/// are evictable. The live hub (`coxagent`) and shared backing infra (`cox-infra`)
/// are NEVER touched — tearing those down to free :8101 would be a self-inflicted
/// outage, and they bind port 4000 anyway (see AGENTS.md). Anything outside our own
/// namespace is foreign and left alone; raw non-compose containers are stopped by id.
fn reclaimable(project: &str) -> bool {
    let lower = project.to_ascii_lowercase();
    if lower == "cox-infra"
        || lower == "coxagent"
        || lower.starts_with("coxagent")
        || lower.starts_with("cox-infra")
    {
        return false;
    }
    lower.starts_with("cox-")
}

/// The container ids of every running container publishing PORT.
fn holders_on_the(port: u16) -> Vec<String> {
    let out = Command::new("docker")
        .args(["ps", "-q", "--filter", &format!("publish={port}")])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Exclusive, ephemeral ownership of the stack for one run: brought up here,
/// torn down when this value drops, even on panic, so a failing run never
/// leaves a container holding the host port.
struct Stack {
    root: PathBuf,
}

impl Stack {
    /// Claims host port HOST_PORT for THIS build's verification pass.
    ///
    /// Tears down whatever currently holds the port before `up` — both anything
    /// left under this directory's compose project by an earlier aborted run and
    /// any FOREIGN agent-preview stack squatting :8101 (see [`clear_host_port`]) —
    /// so the probe below can only ever reach an image freshly built from THIS
    /// tree, never a stale unrelated container answering 200 there. Symmetric with
    /// [`Drop`]; each claim starts from nothing.
    fn claim(root: PathBuf) -> Self {
        let stack = Self { root };
        clear_host_port();
        stack.down();
        stack
    }

    fn down(&self) {
        let _ = compose(&self.root, &["down", "-v"]);
    }
}

/// What an automated pass should do about one container squatting HOST_PORT, as
/// a pure function of that container's compose-project ownership label so every
/// branch is deterministically testable without invoking docker.
#[derive(Debug, PartialEq)]
enum Eviction {
    /// The holder belongs to a reclaimable agent-preview compose project —
    /// tear that whole project down to release the port cleanly.
    ComposeProject(String),
    /// The holder has no compose label (a raw `docker run`) — stop just it by id.
    RawContainer(String),
}

fn classify_holder(owner: Option<&str>, id: String) -> Option<Eviction> {
    match owner {
        Some(project) if reclaimable(project) => Some(Eviction::ComposeProject(project.to_owned())),
        // A protected/foreign compose project is never touched; there may be another
        // squatter sharing :8101 (IPv4+IPv6), so this returns only this one's verdict.
        Some(_non_reclaimable) => None,
        None => Some(Eviction::RawContainer(id)),
    }
}

fn inspect_project_of(id: &str) -> Option<String> {
    Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{ index .Config.Labels \"com.docker.compose.project\" }}",
            id,
        ])
        .output()
        .ok()
        .and_then(|o| {
            let name = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            (!name.is_empty()).then_some(name)
        })
}

fn execute_eviction(action: Eviction) {
    match action {
        Eviction::ComposeProject(project) => {
            let _ = Command::new("docker")
                .args(["compose", "-p", &project, "down", "--remove-orphans"])
                .output();
        }
        Eviction::RawContainer(id) => {
            let _ = Command::new("docker").args(["stop", &id]).output();
        }
    }
}

fn clear_host_port() {
    let port = effective_host_port();
    for id in holders_on_the(port) {
        if let Some(action) = classify_holder(inspect_project_of(&id).as_deref(), id.clone()) {
            execute_eviction(action);
        }
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.down();
    }
}

/// The HTTP status the hub answers with on the published port, or `None`
/// while it is not answering yet.
async fn probe(client: &reqwest::Client, host_port: u16) -> Option<u16> {
    client
        .get(format!("http://localhost:{host_port}/"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()
        .map(|r| r.status().as_u16())
}

#[tokio::test]
#[ignore = "builds a release image and binds host port 8101; run with --ignored"]
async fn compose_stack_comes_up_and_answers_on_the_published_port() {
    let root = repo_root();
    let stack = Stack::claim(root.clone());

    // The exact command docker-compose.yml documents. A compile error in the
    // builder stage surfaces here as a non-zero exit — the original bug.
    let up = compose(&root, &["up", "-d", "--build"]);
    assert!(
        up.status.success(),
        "`docker compose up -d --build` failed ({}):\n{}",
        up.status,
        String::from_utf8_lossy(&up.stderr)
    );

    // Probe whatever this run actually orchestrated — the effective
    // `${APP_PORT:-8101}` fallback — not a constant that could point at an
    // unrelated container squatting on the default port (CXA-B077).
    let host_port = effective_host_port();
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let mut last = None;
    while std::time::Instant::now() < deadline {
        last = probe(&client, host_port).await;
        if last == Some(200) {
            drop(stack);
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let logs = compose(&stack.root, &["logs", "--no-color", "--tail", "50"]);
    panic!(
        "hub never answered 200 on host port {host_port} (last status: {last:?})\n{}",
        String::from_utf8_lossy(&logs.stdout)
    );
}

// ---------------------------------------------------------------------------
// The pure resolver, run against synthetic values — proves the probe always
// tracks what docker compose actually binds, default or override (CXA-B077).
// ---------------------------------------------------------------------------

#[test]
fn unset_app_port_resolves_to_the_assigned_default() {
    assert_eq!(resolve_host_port(None), DEFAULT_HOST_PORT);
}

#[test]
fn empty_app_port_falls_back_to_the_assigned_default() {
    // Compose treats an empty VAR like unset for `${VAR:-default}` too.
    assert_eq!(resolve_host_port(Some("")), DEFAULT_HOST_PORT);
}

#[test]
fn an_override_is_probed_as_orchestrated() {
    assert_eq!(resolve_host_port(Some("8110")), 8110);
}

#[test]
fn a_bad_override_panics_rather_than_guessing_a_probe_target() {
    let caught = std::panic::catch_unwind(|| resolve_host_port(Some("not-a-port")));
    let msg = match caught {
        Err(payload) => payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or_default()
            .to_string(),
        Ok(_) => panic!("a non-numeric APP_PORT must not silently probe a guessed port"),
    };
    assert!(msg.contains("APP_PORT"), "unhelpful panic message: {msg}");
}

/// The eviction decision is pure over an ownership label; verify every branch
/// without invoking docker, and pin that protected infrastructure can never be
/// classified as downed — a regression here would be a self-inflicted outage.
#[cfg(test)]
mod classify_tests {
    use super::{classify_holder, Eviction};

    #[test]
    fn reclaimable_agent_preview_is_torn_down_as_a_project() {
        assert_eq!(
            classify_holder(Some("cox--other-worktree"), "abc".to_owned()),
            Some(Eviction::ComposeProject("cox--other-worktree".to_owned()))
        );
    }

    #[test]
    fn foreign_non_preview_project_is_left_alone() {
        assert_eq!(
            classify_holder(Some("someone-elses-stack"), "abc".to_owned()),
            None
        );
    }

    #[test]
    fn live_hub_and_infra_prefixes_are_never_touched() {
        // The same casing-spoofing set production deploy's shared policy guards
        // against (reclaimable.rs), mirrored here so drift can never reintroduce
        // an automated teardown of the control plane.
        for owner in [
            "coxagent",
            "coxagent-gateway",
            "coxagent-db",
            "cox-infra",
            "cox-infra-db",
            "COXAGENT",
            "CoxAgent",
            "CoxAgent-Gateway",
            "COXAGENT-GATEWAY",
            "COX-INFRA",
            "COX-Infra-DB",
            "Cox-Infra-Db",
        ] {
            assert_eq!(
                classify_holder(Some(owner), "abc".to_owned()),
                None,
                "{owner} must never be evicted"
            );
        }
    }

    #[test]
    fn raw_container_with_no_label_is_stopped_by_id() {
        assert_eq!(
            classify_holder(None, "deadbeef".to_owned()),
            Some(Eviction::RawContainer("deadbeef".to_owned()))
        );
    }
}
