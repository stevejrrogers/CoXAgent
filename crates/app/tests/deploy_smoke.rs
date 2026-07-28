//! COX-B006/COX-B008 regression guard, deploy half: a green `cargo build` is
//! not a running app.
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
//! "The app answered" is checked three ways, because each catches a different
//! half-up stack: EVERY service the compose file defines must be running (a
//! dependency that crashed on boot leaves the hub degraded but still serving),
//! every service that declares a healthcheck must reach `healthy` (Postgres
//! accepts TCP long before it accepts queries), and the hub's own health
//! endpoint must report ok on the published host port.
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

/// The published host port. Fixed by this project's deploy config — see the
/// header comment in docker-compose.yml.
const HOST_PORT: u16 = 8101;

/// The hub's own health endpoint, and the verdict it reports when the app is
/// really serving (not just holding the socket open).
const HEALTH_PATH: &str = "/api/health";
const HEALTHY_BODY: &str = "\"status\":\"ok\"";

/// How long the stack gets to build and answer before the test gives up.
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(3);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Environment that forces Docker's pre-BuildKit builder. Only ever used as a
/// fallback — see [`bring_stack_up`].
const CLASSIC_BUILDER: [(&str, &str); 2] =
    [("DOCKER_BUILDKIT", "0"), ("COMPOSE_DOCKER_CLI_BUILD", "0")];

fn compose(root: &Path, args: &[&str]) -> std::process::Output {
    compose_with(root, args, &[])
}

fn compose_with(root: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new("docker");
    cmd.arg("compose")
        .args(args)
        .current_dir(root)
        // The documented bring-up command sets the admin password inline.
        .env("COXAGENT_ADMIN_PASSWORD", "ci-smoke");
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output()
        .unwrap_or_else(|e| panic!("`docker compose {}` failed to spawn: {e}", args.join(" ")))
}

/// Whether `text` is BuildKit failing to bookkeep *itself* rather than
/// anything in this repo failing to build. Seen on dev hosts as
///
/// ```text
/// failed to update builder last activity time: open
/// ~/.docker/buildx/activity/.tmp-default1234: operation not permitted
/// ```
///
/// which aborts the run before a single Dockerfile step executes. Telling it
/// apart from a real build failure is load-bearing for this ticket: read as a
/// product failure it looks exactly like the compile regression this test
/// exists to catch, so a broken host can "reproduce" COX-B006 on a tree that
/// already has the fix — and, worse, hide a later real one in the same noise.
fn is_buildkit_host_fault(text: &str) -> bool {
    text.contains("failed to update builder last activity time")
}

/// Run the exact bring-up command docker-compose.yml documents, retrying on
/// the classic builder when BuildKit is unusable on this host. The retry
/// changes *which builder assembles the image*, never what is built from it,
/// so a genuine compile error still fails both attempts and still fails the
/// test.
fn output_text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn bring_stack_up(root: &Path) -> std::process::Output {
    let up = compose(root, &["up", "-d", "--build"]);
    if up.status.success() {
        return up;
    }
    let why = output_text(&up);
    if !is_buildkit_host_fault(&why) {
        return up;
    }
    eprintln!("BuildKit is unusable on this host; retrying on the classic builder:\n{why}");
    let classic = compose_with(root, &["up", "-d", "--build"], &CLASSIC_BUILDER);
    if classic.status.success() {
        return classic;
    }
    // On some Docker/Compose versions `DOCKER_BUILDKIT=0` /
    // `COMPOSE_DOCKER_CLI_BUILD=0` no longer stop `compose build` from
    // shelling out to buildx bake, so the classic-builder retry hits the
    // exact same activity-dir permission fault and gains nothing. Pointing
    // `BUILDX_CONFIG` at a writable, per-run directory keeps BuildKit itself
    // (fixing the actual permission fault instead of trying to avoid it) —
    // confirmed to bring the stack up on a host where the classic-builder
    // retry above did not.
    let why2 = output_text(&classic);
    if !is_buildkit_host_fault(&why2) {
        return classic;
    }
    eprintln!("Classic builder retry hit the same host fault; redirecting BUILDX_CONFIG:\n{why2}");
    let buildx_dir = std::env::temp_dir().join(format!(
        "coxagent-deploy-smoke-buildx-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&buildx_dir);
    compose_with(
        root,
        &["up", "-d", "--build"],
        &[(
            "BUILDX_CONFIG",
            buildx_dir.to_str().expect("temp dir path is valid UTF-8"),
        )],
    )
}

/// Tears the stack down even when the test panics, so a failing run never
/// leaves a container holding the host port.
struct Stack {
    root: PathBuf,
}

impl Drop for Stack {
    fn drop(&mut self) {
        let _ = compose(&self.root, &["down", "-v"]);
    }
}

/// The status and body the hub answers with for `path` on the published port,
/// or `None` while it is not answering yet.
async fn probe(client: &reqwest::Client, path: &str) -> Option<(u16, String)> {
    let response = client
        .get(format!("http://localhost:{HOST_PORT}{path}"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    let status = response.status().as_u16();
    Some((status, response.text().await.unwrap_or_default()))
}

/// Every service docker-compose.yml defines, whether or not it is up — the
/// set "every service reaches a running state" is measured against. Read from
/// the compose file itself so a service added later is covered automatically.
fn defined_services(root: &Path) -> Vec<String> {
    let out = compose(root, &["config", "--services"]);
    assert!(
        out.status.success(),
        "`docker compose config --services` failed ({}):\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// The lifecycle status of `service`'s container plus its healthcheck verdict
/// (`"none"` when the service declares no healthcheck), or `None` while
/// compose has not created the container yet. Asked of `docker inspect`
/// rather than parsed out of `compose ps --format json`, whose shape has
/// changed between Compose releases.
fn service_state(root: &Path, service: &str) -> Option<(String, String)> {
    // `--all`: a container that crashed on boot must be reported as `exited`,
    // not silently read as "not created yet" until the deadline runs out.
    let ids = compose(root, &["ps", "--all", "--quiet", service]);
    let id = String::from_utf8_lossy(&ids.stdout).trim().to_owned();
    if id.is_empty() {
        return None;
    }
    let out = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}",
            &id,
        ])
        .output()
        .unwrap_or_else(|e| panic!("`docker inspect {id}` failed to spawn: {e}"));
    let state = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let (status, health) = state.split_once(' ')?;
    Some((status.to_owned(), health.to_owned()))
}

/// `Ok(())` once every service is running and every declared healthcheck
/// passes; otherwise why the stack is not there yet, for the failure message.
fn services_ready(root: &Path, services: &[String]) -> Result<(), String> {
    for service in services {
        match service_state(root, service) {
            None => return Err(format!("service `{service}` has no container")),
            Some((status, _)) if status != "running" => {
                return Err(format!("service `{service}` is `{status}`, not running"))
            }
            Some((_, health)) if health != "none" && health != "healthy" => {
                return Err(format!(
                    "service `{service}` is running but its healthcheck says `{health}`"
                ))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "builds a release image and binds host port 8101; run with --ignored"]
async fn compose_stack_comes_up_and_answers_on_the_published_port() {
    let root = repo_root();
    let stack = Stack { root: root.clone() };

    let services = defined_services(&root);
    assert!(
        !services.is_empty(),
        "docker-compose.yml defines no services — the smoke test would pass vacuously"
    );

    // The exact command docker-compose.yml documents. A compile error in the
    // builder stage surfaces here as a non-zero exit — the original bug.
    let up = bring_stack_up(&root);
    assert!(
        up.status.success(),
        "`docker compose up -d --build` failed ({}):\n{}",
        up.status,
        String::from_utf8_lossy(&up.stderr)
    );

    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let mut pending = "the stack was never polled".to_owned();
    while std::time::Instant::now() < deadline {
        pending = match services_ready(&root, &services) {
            Err(why) => why,
            // Only once every container is up does a silent health endpoint
            // mean the APP is broken rather than the stack still booting.
            Ok(()) => match probe(&client, HEALTH_PATH).await {
                Some((200, body)) if body.contains(HEALTHY_BODY) => {
                    let dashboard = probe(&client, "/").await.map(|(status, _)| status);
                    assert_eq!(
                        dashboard,
                        Some(200),
                        "{HEALTH_PATH} reports ok but the dashboard at / answered {dashboard:?}"
                    );
                    drop(stack);
                    return;
                }
                Some((status, body)) => format!(
                    "{HEALTH_PATH} answered {status} with {:?}",
                    body.chars().take(120).collect::<String>()
                ),
                None => format!("{HEALTH_PATH} is not answering on host port {HOST_PORT}"),
            },
        };
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let logs = compose(&stack.root, &["logs", "--no-color", "--tail", "50"]);
    panic!(
        "stack never came up within {READY_TIMEOUT:?}: {pending}\n{}",
        String::from_utf8_lossy(&logs.stdout)
    );
}

/// Runs without Docker, so the one piece of judgement in this file — "was that
/// the host or was that us?" — is covered on every `cargo test`, not only in
/// the `deploy-smoke` job.
#[test]
fn a_broken_buildkit_is_told_apart_from_a_broken_build() {
    assert!(
        is_buildkit_host_fault(
            "failed to update builder last activity time: open \
             /Users/dev/.docker/buildx/activity/.tmp-default3914110947: operation not permitted"
        ),
        "BuildKit's own bookkeeping failure must be recognised as a host fault"
    );

    // COX-B006 itself. Excusing this as a host fault would let a Docker image
    // that cannot compile ship green — the exact regression under guard.
    assert!(
        !is_buildkit_host_fault(
            "error: function `seatbelt_profile` is never used\n\
             error: could not compile `coxagent-infrastructure` (lib) due to 1 previous error\n\
             failed to solve: process \"/bin/sh -c cargo build --release\" did not complete \
             successfully: exit code: 101"
        ),
        "a compile error in the builder stage must never be excused as a host fault"
    );
}
