//! COX-B043 guard: a registered project the hub could not load must still be
//! VISIBLE in `GET /api/projects`, flagged broken and carrying the reason.
//!
//! Before this, a project whose `coxagent.json` failed to parse was dropped
//! from the hub's project map, so the listing simply omitted it: the dashboard
//! showed a project that had vanished, and the only trace of why was a line in
//! the hub log. The flag matters as much as the row — every other route behind
//! a project assumes a store and a runner, so a client must be able to list a
//! broken project without offering it as selectable.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

/// The reason surfaced to the user — the shape `load_config` produces for the
/// ticket's own repro (`"deploy":{"host_port":999999}`), field named.
const REASON: &str = "invalid /w/broken/coxagent.json: deploy.host_port: invalid value: integer `999999`, expected u16";

/// Boot a hub with nothing but one unloadable project, and return its port
/// (plus the temp dir keeping the hub's own files out of the source tree —
/// `hub_dir: None` would put its nightly backups in the crate directory).
/// No auth is wired: this test is about the listing's content, and the auth
/// gate has its own guards.
async fn boot() -> (u16, tempfile::TempDir) {
    let hub_dir = tempfile::tempdir().expect("tempdir");
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve a free port")
        .local_addr()
        .expect("local addr")
        .port();
    let extras = coxagent_presentation::HubExtras {
        hub_dir: Some(hub_dir.path().to_path_buf()),
        broken: vec![coxagent_presentation::BrokenProject {
            id: "broken".to_owned(),
            config_path: std::path::PathBuf::from("/w/broken/coxagent.json"),
            error: REASON.to_owned(),
        }],
        ..Default::default()
    };
    let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
        Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
    tokio::spawn(coxagent_presentation::serve_full(
        vec![],
        port,
        audit,
        extras,
    ));
    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{port}/api/health");
    for _ in 0..50 {
        if client.get(&health).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    (port, hub_dir)
}

#[tokio::test]
async fn a_project_that_failed_to_load_is_listed_as_broken_with_its_reason() {
    let (port, _hub_dir) = boot().await;

    let body: serde_json::Value = reqwest::get(format!("http://127.0.0.1:{port}/api/projects"))
        .await
        .expect("list projects")
        .json()
        .await
        .expect("projects listing is JSON");

    let entries = body.as_array().expect("the listing is an array");
    assert_eq!(entries.len(), 1, "the broken project must not be omitted");
    assert_eq!(entries[0]["id"], "broken");
    assert_eq!(
        entries[0]["broken"], true,
        "a client must be able to tell a broken project from a live one"
    );
    assert_eq!(
        entries[0]["error"], REASON,
        "the reason must reach the dashboard, not just the hub log"
    );
    assert_eq!(
        entries[0]["config_path"], "/w/broken/coxagent.json",
        "the listing must name the file to fix"
    );
}
