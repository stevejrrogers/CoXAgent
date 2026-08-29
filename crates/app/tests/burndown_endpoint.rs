//! CXA-F032 endpoint contract: GET /api/projects/:pid/metrics/burndown.
//!
//! Same conventions as `agent_cycle_metrics_dashboard.rs`: a hub boots with one
//! project backed by an in-memory store so we assert real HTTP behaviour — the
//! payload shape (`series` of per-day `{day, open, fixed, verified}` plus
//! `delta_24h`), recorded snapshots beating live counts, the missing-project
//! 404, and the OpenAPI registration every `/api/*` route must carry.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::config::BudgetCaps;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::state::BugSnapshot;
use coxagent_application::{PortError, ProjectState};
use coxagent_domain::{Complexity, Priority, Status, Ticket, TicketId, TicketType};
use coxagent_presentation::{HubExtras, ProjectHandle};

const PORT: u16 = 47_841;

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
        unreachable!("metrics endpoints never run an engine")
    }
}

fn handle(id: &str, state: ProjectState) -> ProjectHandle {
    let dir = std::env::temp_dir();
    ProjectHandle {
        id: id.to_owned(),
        name: id.to_owned(),
        alias: id.to_owned(),
        outbox: None,
        store: Arc::new(MemStore {
            state: Mutex::new(state),
        }),
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

/// `YYYY-MM-DD` for today minus `back` days — the same UTC-day convention the
/// endpoint itself uses.
fn day_back(back: i64) -> String {
    time::OffsetDateTime::now_utc()
        .saturating_sub(time::Duration::days(back))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()[..10]
        .to_owned()
}

fn bug(id: &str) -> Ticket {
    Ticket::new(
        TicketId::new(id).unwrap(),
        TicketType::Bug,
        format!("defect {id}"),
        "repro",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .unwrap()
}

/// Two recorded days of history + a live backlog of 2 open / 1 verified bugs.
fn seeded_state() -> ProjectState {
    let mut s = ProjectState::default();
    for id in ["B2301", "B2302"] {
        s.tickets.push(bug(id));
    }
    // One already-verified bug, driven along the only legal route.
    let mut v = bug("B2303");
    v.transition_to(coxagent_domain::Role::DevBug, Status::InProgress)
        .unwrap();
    v.transition_to(coxagent_domain::Role::DevBug, Status::Fixed)
        .unwrap();
    v.transition_to(coxagent_domain::Role::Test, Status::Verified)
        .unwrap();
    s.tickets.push(v);
    s.bug_snapshots.insert(
        day_back(2),
        BugSnapshot {
            open: 7,
            fixed: 2,
            verified: 2,
        },
    );
    s.bug_snapshots.insert(
        day_back(1),
        BugSnapshot {
            open: 5,
            fixed: 1,
            verified: 2,
        },
    );
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
        handles, PORT, audit, extras,
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
async fn burndown_endpoint_shape_history_and_404() {
    let _dir = boot(vec![handle("demo", seeded_state())]).await;
    let client = reqwest::Client::new();

    let url = format!("http://127.0.0.1:{PORT}/api/projects/demo/metrics/burndown");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();

    // Payload shape exactly as the CXA-F032 contract pins it.
    let series = v["series"].as_array().expect("series array");
    assert!(v["delta_24h"].is_i64(), "delta_24h is an int");
    assert!(series.len() >= 3, "recorded days + today, got {series:?}");
    for point in series {
        assert!(point["day"].as_str().is_some_and(|d| d.len() == 10));
        assert!(point["open"].is_u64());
        assert!(point["fixed"].is_u64());
        assert!(point["verified"].is_u64());
    }

    // Recorded history beats live counts, oldest first, and today closes the
    // series with the LIVE counts (2 open / 0 fixed / 1 verified).
    let days: Vec<&str> = series.iter().map(|p| p["day"].as_str().unwrap()).collect();
    let yesterday = day_back(1);
    let today = day_back(0);
    assert_eq!(days.last().copied(), Some(today.as_str()));
    assert!(days.contains(&yesterday.as_str()));
    let y = series
        .iter()
        .find(|p| p["day"] == yesterday.as_str())
        .unwrap();
    assert_eq!(y["open"], 5, "recorded snapshot wins over live state");
    let t = series.last().unwrap();
    assert_eq!(t["open"], 2, "today uses the live counts");
    assert_eq!(t["verified"], 1);

    // delta_24h = prev.open - latest.open = 5 - 2.
    assert_eq!(v["delta_24h"], 3, "net burned today is positive");

    // Missing project: 404, matching every other project-scoped route.
    let url = format!("http://127.0.0.1:{PORT}/api/projects/nope/metrics/burndown");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);

    // The route is registered in the OpenAPI document (CXA-C005 drift guard).
    let url = format!("http://127.0.0.1:{PORT}/api/openapi.json");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let doc: serde_json::Value = resp.json().await.unwrap();
    let op = &doc["paths"]["/api/projects/:pid/metrics/burndown"]["get"];
    assert!(
        op.is_object(),
        "burndown route must be registered in the OpenAPI spec"
    );
}
