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

/// Exclusive, ephemeral ownership of the stack for one run: brought up here,
/// torn down when this value drops, even on panic, so a failing run never
/// leaves a container holding the host port.
struct Stack {
    root: PathBuf,
}

impl Stack {
    /// Claims the stack, tearing down anything already up under this compose
    /// project first.
    ///
    /// `Drop` only cleans up after *this* run. A stack left behind by anything
    /// that skipped it — a `docker compose up -d --build` run by hand while
    /// debugging, a `kill -9`'d test — keeps holding host port 8101, and the
    /// `up` below then either fails to bind or silently reuses the stale
    /// containers. The test would go on to probe an image built from someone
    /// else's tree and report its health as this commit's. Teardown before
    /// `up`, symmetric with `Drop`, makes each run start from nothing.
    fn claim(root: PathBuf) -> Self {
        let stack = Self { root };
        stack.down();
        stack
    }

    fn down(&self) {
        let _ = compose(&self.root, &["down", "-v"]);
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
