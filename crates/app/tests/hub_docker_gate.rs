//! CXA-B153 regression guard: the deployed hub must be able to drive the HOST
//! docker daemon.
//!
//! The bug: the B143 docker janitor (and the compose deploy adapter) shell out
//! to `docker` — but the hub ships as a container on :8101 whose image carried
//! no docker CLI and whose service mounted no socket. Every probe failed and
//! every sweep is fail-closed, so thirteen hours of hourly ticks reclaimed
//! nothing: the fix was "in the build" and dead in production, which is
//! exactly the CXA-B149 complaint all over again.
//!
//! Two independent halves must hold, so either one can silently regress:
//!   1. the image ships the docker CLI + compose plugin (Dockerfile);
//!   2. the root compose service that publishes the hub's port mounts the
//!      host socket, writable.
//!
//! Both guards are pure functions over repo source returning `Err`; the tests
//! run them against the real files and against synthetic drift to prove each
//! rule bites independently.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// The guards: pure functions over the Dockerfile / compose source.
// ---------------------------------------------------------------------------

/// `Ok` only when the hub image ships the `docker` binary AND the compose
/// plugin — the janitor runs `docker compose` too, so either copy going away
/// resurrects the silent no-op.
fn ships_docker_cli(dockerfile_src: &str) -> Result<(), String> {
    const REQUIRED_COPIES: &[&str] = &[
        "COPY --from=docker:cli /usr/local/bin/docker",
        "COPY --from=docker:cli /usr/local/libexec/docker/cli-plugins/",
    ];
    let missing: Vec<&str> = REQUIRED_COPIES
        .iter()
        .copied()
        .filter(|needle| !dockerfile_src.contains(needle))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Dockerfile no longer ships the host-docker toolchain (CXA-B153): missing \
         {missing:?}. The hub container shells out to `docker` (and `docker compose`) \
         against the HOST daemon; an image without the CLI fails every probe and the \
         fail-closed janitor skips every sweep — the fix is in the build and dead in \
         production"
    ))
}

/// Why the service cannot reach the host daemon; `None` when it can.
fn unreachable_host_docker_reason(svc: &serde_yaml::Value) -> Option<String> {
    let no_volumes: Vec<serde_yaml::Value> = Vec::new();
    for volume in svc["volumes"].as_sequence().unwrap_or(&no_volumes) {
        if let Some(mount) = volume.as_str() {
            // Short syntax `HOST:CONTAINER[:MODE]`: no mode suffix and an
            // explicit `:rw` are both writable; only `:ro` is not.
            if mount.starts_with("/var/run/docker.sock:") && !mount.ends_with(":ro") {
                return None;
            }
        }
        // Long syntax: {type: bind, source: HOST, target: CONTAINER, read_only: BOOL}.
        if volume["source"].as_str() == Some("/var/run/docker.sock")
            && volume["read_only"].as_bool() != Some(true)
        {
            return None;
        }
    }
    Some(
        "no writable /var/run/docker.sock mount — every docker probe fails and the \
         fail-closed janitor skips every sweep (CXA-B153)"
            .to_string(),
    )
}

/// `Ok` only when the compose service that publishes the hub's container port
/// 4000 mounts the host docker socket writable.
fn hub_docker_reachability(compose_src: &str) -> Result<(), String> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(compose_src).map_err(|e| format!("docker-compose.yml: {e}"))?;
    let services = doc["services"]
        .as_mapping()
        .ok_or_else(|| "docker-compose.yml: no services map".to_string())?;
    for (name, svc) in services {
        let name = name.as_str().unwrap_or("<unnamed>");
        let publishes_hub_port = svc["ports"]
            .as_sequence()
            .is_some_and(|ports| {
                ports.iter().any(|port| {
                    port.as_str()
                        .is_some_and(|p| p.trim_matches('"').ends_with(":4000"))
                        || port["target"].as_str().is_some_and(|t| t == "4000")
                        || port["target"].as_i64() == Some(4000)
                })
            });
        if !publishes_hub_port {
            continue;
        }
        if let Some(why) = unreachable_host_docker_reason(svc) {
            return Err(format!("services.{name}: {why}"));
        }
        return Ok(());
    }
    Err(
        "docker-compose.yml: no service publishes container port 4000 — the hub-docker \
         reachability guard is blind"
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// The guards, run against the real repo files.
// ---------------------------------------------------------------------------

#[test]
fn the_hub_image_ships_the_docker_cli_and_compose_plugin() {
    if let Err(why) = ships_docker_cli(&read("Dockerfile")) {
        panic!("{why}");
    }
}

#[test]
fn the_hub_service_mounts_the_host_docker_socket() {
    if let Err(why) = hub_docker_reachability(&read("docker-compose.yml")) {
        panic!("{why}");
    }
}

// ---------------------------------------------------------------------------
// Synthetic drift — proves each rule bites independently.
// ---------------------------------------------------------------------------

/// A minimal compose whose coxagent service publishes the hub port and mounts
/// the given volume (or none).
fn compose_with_hub_volumes(volume: Option<&str>) -> String {
    let volume_line = volume
        .map(|v| format!("      - {v}\n"))
        .unwrap_or_default();
    format!(
        "services:\n  coxagent:\n    image: coxagent\n    ports:\n\
         \x20   - \"${{APP_PORT:-8101}}:4000\"\n    volumes:\n{volume_line}"
    )
}

/// The exact pre-fix shape that shipped the bug: CLI in the image, no socket.
#[test]
fn a_compose_without_the_socket_is_caught() {
    let why = hub_docker_reachability(&compose_with_hub_volumes(None)).unwrap_err();
    assert!(why.contains("docker.sock"), "unhelpful message: {why}");
    assert!(why.contains("CXA-B153"), "message must cite the ticket: {why}");
}

/// A read-only socket authenticates nothing the janitor needs: it cannot
/// remove containers, volumes or images, so `:ro` is the bug in disguise.
#[test]
fn a_read_only_socket_mount_is_caught() {
    let src = compose_with_hub_volumes(Some("/var/run/docker.sock:/var/run/docker.sock:ro"));
    let why = hub_docker_reachability(&src).unwrap_err();
    assert!(why.contains("writable"), "unhelpful message: {why}");
}

/// The shipped, fixed shape must pass cleanly.
#[test]
fn a_writable_socket_mount_passes() {
    let src = compose_with_hub_volumes(Some("/var/run/docker.sock:/var/run/docker.sock"));
    assert_eq!(
        hub_docker_reachability(&src),
        Ok(()),
        "the fixed mount must pass"
    );
}

/// Long-syntax mounts are equally valid compose — the guard must read them.
#[test]
fn the_long_form_socket_mount_passes() {
    let src = "services:\n  coxagent:\n    image: coxagent\n    ports:\n\
               \x20   - \"8101:4000\"\n    volumes:\n      - type: bind\n\
               \x20       source: /var/run/docker.sock\n\
               \x20       target: /var/run/docker.sock\n";
    assert_eq!(hub_docker_reachability(src), Ok(()));
}

/// A read-only LONG-form mount is the same bug in the other syntax.
#[test]
fn a_long_form_read_only_mount_is_caught() {
    let src = "services:\n  coxagent:\n    image: coxagent\n    ports:\n\
               \x20   - \"8101:4000\"\n    volumes:\n      - type: bind\n\
               \x20       source: /var/run/docker.sock\n\
               \x20       target: /var/run/docker.sock\n        read_only: true\n";
    let why = hub_docker_reachability(src).unwrap_err();
    assert!(why.contains("writable"), "unhelpful message: {why}");
}

/// No service publishing 4000 leaves the guard blind — fail loudly, never
/// bless nothing.
#[test]
fn a_compose_without_the_hub_service_is_reported() {
    let src = "services:\n  db:\n    image: postgres:16-alpine\n";
    let why = hub_docker_reachability(src).unwrap_err();
    assert!(why.contains("blind"), "unhelpful message: {why}");
}

/// Dropping just the CLI copy resurrects the silent no-op half.
#[test]
fn a_dockerfile_without_the_cli_copy_is_caught() {
    let src = "FROM debian:bookworm-slim\n\
               COPY --from=docker:cli /usr/local/libexec/docker/cli-plugins/ \
               /usr/local/libexec/docker/cli-plugins/\n";
    let why = ships_docker_cli(src).unwrap_err();
    assert!(
        why.contains("/usr/local/bin/docker"),
        "unhelpful message: {why}"
    );
    assert!(why.contains("CXA-B153"), "message must cite the ticket: {why}");
}

/// Dropping just the compose plugin breaks `docker compose …` probes only —
/// subtler, so it gets its own bite.
#[test]
fn a_dockerfile_without_the_compose_plugin_is_caught() {
    let src = "FROM debian:bookworm-slim\nCOPY --from=docker:cli /usr/local/bin/docker \
               /usr/local/bin/docker\n";
    let why = ships_docker_cli(src).unwrap_err();
    assert!(why.contains("cli-plugins"), "unhelpful message: {why}");
}
