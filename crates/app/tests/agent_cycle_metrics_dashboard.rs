//! CXA-F018 endpoint contract: /metrics/summary and /metrics/trends.
//!
//! A hub boots with one project backed by an in-memory store so we assert real
//! HTTP behaviour: valid project 200, missing project 404, and fewer than three
//! completed cycles reporting "(insufficient data)".

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::config::BudgetCaps;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::CycleScore;
use coxagent_application::{PortError, ProjectState};
use coxagent_presentation::{HubExtras, ProjectHandle};

const PORT: u16 = 47_834;

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
    fn id(&self) -> &'static str { "stub" }
    async fn run(&self, _rq: AgentRequest) -> Result<AgentOutcome, PortError> {
        unreachable!("metrics endpoints never run an engine")
    }
}

fn handle(id: &str, state: ProjectState) -> ProjectHandle {
    let dir = std::env::temp_dir();
    ProjectHandle {
        id: id.to_owned(),
        name: id.to_owned(),
        alias: id.to_owned(),
        store: Arc::new(MemStore { state: Mutex::new(state) }),
        runner: Arc::new(coxagent_application::use_cases::RunnerHandle::new()),
        config_path: dir.join("coxagent.json"),
        engine: Arc::new(StubEngine),
        work_dir: dir.clone(),
        budget: Arc::new(Mutex::new(BudgetCaps::default())),
        context_path: dir.join("context.md"),
        forge: None,
        deploy: None,
        files: None,
        storage: None,
    }
}

fn seeded_state(cycles_n: usize) -> ProjectState {
    let mut s = ProjectState::default();
    for i in 0..cycles_n {
        let cs: CycleScore = serde_json::from_value(serde_json::json!({
            "cycle": (i as u64) + 1,
            "at": "2026-08-01T00:00:00Z",
            "runs": 1, "useful": 1, "cost_usd": 1.0,
            "shipped": 1, "incidents": 0, "errors": 0, "grade": "B"
        })).unwrap();
        s.cycle_scores.push(cs);
    }
    s
}

async fn boot(handles: Vec<ProjectHandle>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let extras = HubExtras {
        hub_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
        Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
    tokio::spawn(coxagent_presentation::serve_full(
        handles,
        PORT,
        audit,
        extras,
    ));
    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{PORT}/api/health");
    for _ in 0..50 {
        if client.get(&health).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    dir
}

#[tokio::test]
async fn metrics_endpoints_return_shape_404_and_insufficient_edge() {
    let _dir = boot(vec![
        handle("demo", seeded_state(3)),
        handle("fresh", seeded_state(0)),
    ])
    .await;
    let client = reqwest::Client::new();

    let url = format!("http://127.0.0.1:{PORT}/api/projects/demo/metrics/summary");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v.get("velocity_by_sprint").is_some());
    assert!(v.get("burn_rate").is_some());
    assert!(v.get("cost_by_outcome_type").is_some());
    // AC5: a burn-warning absent for normal spend serializes as JSON null, so
    // the dashboard can omit the badge without special-casing a missing key.
    assert!(v["burn_warning"].is_null());
    // Pattern recognition must surface total failure volume, not just top-3.
    assert_eq!(v["patterns"]["total_failures"], 0);

    // Missing project: 404, matching every other project-scoped route.
    let url = format!("http://127.0.0.1:{PORT}/api/projects/nope/metrics/summary");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);

    // Fewer than three completed cycles: trends answer success with the
    // insufficient-data flag (AC5), never a fabricated chart.
    let url = format!("http://127.0.0.1:{PORT}/api/projects/fresh/metrics/trends?days=14");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["insufficient_data"], true);
}
