//! CXA-B128 endpoint contract: the live agent-log SSE stream must kick every
//! fresh open off with an `init` event — even when the role has no live log
//! file at all. Before the fix the server sent nothing until the file grew,
//! so a healthy connection delivered no event and the dashboard's Work-log
//! panel sat on its indefinite "loading…" placeholder forever (no init → no
//! empty state; a connected stream fires no error either).
//!
//! Real HTTP against a booted hub, mirroring agent_cycle_metrics_dashboard.rs.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::config::BudgetCaps;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::{PortError, ProjectState};
use coxagent_presentation::{HubExtras, ProjectHandle};

const PORT: u16 = 47_851;
// Each test boots its own hub; tokio spawns swallow a bind error, so two
// tests sharing a port would silently answer each other's requests.
const PORT_LIVE: u16 = 47_852;

struct MemStore {
    state: Mutex<ProjectState>,
}

#[async_trait]
impl StateStorePort for MemStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().unwrap().clone())
    }
    async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
        *self.state.lock().unwrap() = s.clone();
        Ok(())
    }
}

struct StubEngine;
#[async_trait]
impl AgentEnginePort for StubEngine {
    fn id(&self) -> &'static str {
        "stub"
    }
    async fn run(&self, _rq: AgentRequest) -> Result<AgentOutcome, PortError> {
        unreachable!("the live-log stream never runs an engine")
    }
}

/// A project rooted at its own scratch dir: `config_path.parent()` is where
/// the stream resolver looks for `logs/live/<role>…log`, so each test decides
/// whether that file exists by building the handle on its own tempdir.
fn handle(id: &str, root: &std::path::Path) -> ProjectHandle {
    ProjectHandle {
        id: id.to_owned(),
        name: id.to_owned(),
        alias: id.to_owned(),
        store: Arc::new(MemStore {
            state: Mutex::new(ProjectState::default()),
        }),
        runner: Arc::new(coxagent_application::use_cases::RunnerHandle::new()),
        outbox: None,
        config_path: root.join("coxagent.json"),
        engine: Arc::new(StubEngine),
        work_dir: root.to_path_buf(),
        budget: Arc::new(Mutex::new(BudgetCaps::default())),
        context_path: root.join("context.md"),
        forge: None,
        deploy: None,
        files: None,
        deps_discovery: None,
        storage: None,
    }
}

async fn boot(handles: Vec<ProjectHandle>, port: u16) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let extras = HubExtras {
        hub_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
        Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
    tokio::spawn(coxagent_presentation::serve_full(
        handles, port, audit, extras,
    ));
    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{port}/api/health");
    for _ in 0..50 {
        if client.get(&health).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    dir
}

/// Read SSE frames off the stream until `want` (an event name) arrives, then
/// return the event's `data:` lines — or fail: the whole point is that `init`
/// arrives PROMPTLY, not after the file grows or the 15s keep-alive.
async fn first_event(mut resp: reqwest::Response, want: &str) -> Vec<String> {
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "stream never delivered event `{want}`; frames so far: {buf}"
        );
        let chunk = match tokio::time::timeout_at(deadline, resp.chunk()).await {
            // Timed out: the server never sent the event — the exact stall
            // this contract test exists to catch.
            Err(e) => panic!("timed out waiting for the next SSE chunk ({e}); frames so far: {buf}"),
            Ok(Err(e)) => panic!("stream read failed: {e}; frames so far: {buf}"),
            Ok(Ok(None)) => panic!("stream closed before delivering `{want}`; frames so far: {buf}"),
            Ok(Ok(Some(bytes))) => bytes,
        };
        buf.push_str(&String::from_utf8_lossy(&chunk));
        // A complete frame: `event: <name>\ndata: <json>\n\n`
        if let Some(pos) = buf.find("\n\n") {
            let frame = buf[..pos].to_string();
            let mut event = String::new();
            let mut data = Vec::new();
            for line in frame.lines() {
                if let Some(e) = line.strip_prefix("event: ") {
                    event = e.to_string();
                }
                if let Some(d) = line.strip_prefix("data: ") {
                    data.push(d.to_string());
                }
            }
            if event == want {
                return data;
            }
            buf.clear();
        }
    }
}

#[tokio::test]
async fn fresh_open_with_no_live_log_still_sends_init_promptly() {
    let scratch = tempfile::tempdir().unwrap();
    // No logs/ directory is created at all — the exact deployed shape for a
    // project whose agents have never run.
    let _hub = boot(vec![handle("empty", scratch.path())], PORT).await;
    let client = reqwest::Client::new();

    let url = format!(
        "http://127.0.0.1:{PORT}/api/projects/empty/agent-log/stream?role=dev_feature"
    );
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .starts_with("text/event-stream"));

    let data = first_event(resp, "init").await;
    let v: serde_json::Value =
        serde_json::from_str(data.first().map_or("", String::as_str)).unwrap();
    // live:false is the client's cue to paint its terminal "hasn't run yet"
    // empty state instead of waiting for content that will never come.
    assert_eq!(v["live"], false, "no live log must stream as live:false");
    assert_eq!(v["role"], "dev_feature");
    assert_eq!(v["offset"], 0);
}

#[tokio::test]
async fn fresh_open_with_a_live_log_sends_init_then_the_snapshot() {
    let scratch = tempfile::tempdir().unwrap();
    let live_dir = scratch.path().join("logs").join("live");
    std::fs::create_dir_all(&live_dir).unwrap();
    std::fs::write(
        live_dir.join("dev_feature.log"),
        "# run started\n💬 building the fix\n",
    )
    .unwrap();
    let _hub = boot(vec![handle("live", scratch.path())], PORT_LIVE).await;
    let client = reqwest::Client::new();

    let url = format!(
        "http://127.0.0.1:{PORT_LIVE}/api/projects/live/agent-log/stream?role=dev_feature"
    );
    let resp = client.get(&url).send().await.unwrap();
    let data = first_event(resp, "init").await;
    let v: serde_json::Value =
        serde_json::from_str(data.first().map_or("", String::as_str)).unwrap();
    assert_eq!(v["live"], true, "an existing live log must stream as live:true");

    // Non-regression: the snapshot bytes still follow the init kick-off.
    let url = format!(
        "http://127.0.0.1:{PORT_LIVE}/api/projects/live/agent-log/stream?role=dev_feature"
    );
    let resp = client.get(&url).send().await.unwrap();
    let data = first_event(resp, "line").await;
    let text: String = data.join("\n");
    assert!(
        text.contains("building the fix"),
        "snapshot line event must carry the live log's content, got: {text}"
    );
}
