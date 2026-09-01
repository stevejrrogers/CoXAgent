//! CXA-F275 endpoint contract: GET /api/search.
//!
//! The pure ranking/scoping semantics live in
//! `application/src/use_cases/global_search.rs` (unit-tested there); this file
//! pins the WIRE behavior the palette depends on — the auth gate the bare
//! path bypasses (`auth_mw`'s pid check never fires for `/api/search`, so the
//! handler enforces membership itself), the bare-array hit shape, the
//! short-query refusal, and the OpenAPI registration.
//!
//! Same conventions as `rest_state_store_contract.rs`: the shared
//! `rest_store_support` harness (real `serve_full`, in-memory store, stub
//! auth) — no fake server, no fabricated shapes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

// The shared harness carries helpers this file does not use; the allow is
// per-binary and does not touch the shared module.
#[allow(dead_code)]
mod rest_store_support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use coxagent_application::config::BudgetCaps;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, StateStorePort,
};
use coxagent_application::state::DocPage;
use coxagent_application::{PortError, ProjectState, GENERAL_CHANNEL};
use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};
use coxagent_presentation::ProjectHandle;
use rest_store_support::{boot, StubAuth, MEMBER_BEARER};

/// Ports unique across this crate's hub-booting tests (see the registry note
/// in rest_store_support: 47_735-47_738, 47_712+, 47_841).
const PORT: u16 = 47_870;
const OPEN_PORT: u16 = 47_871;

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
        unreachable!("search never runs an engine")
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
        deps_discovery: None,
        storage: None,
    }
}

/// One of each searchable kind, all mentioning the seeded needle.
fn seeded_state() -> ProjectState {
    let mut s = ProjectState::default();
    s.tickets.push(
        Ticket::new(
            TicketId::new("CXC-F275").unwrap(),
            TicketType::Feature,
            "Fix payment webhook retries",
            "the payment provider drops the connection",
            Priority::High,
            Complexity::Small,
            false,
        )
        .unwrap(),
    );
    s.docs.push(DocPage {
        id: "doc-payments".to_owned(),
        folder: "Product".to_owned(),
        category: "product".to_owned(),
        title: "Payment integration guide".to_owned(),
        body: "configure retries for failed payment hooks".to_owned(),
        updated_at: "2026-08-31T10:00:00Z".to_owned(),
        updated_by: "DOCS".to_owned(),
    });
    s.chat.push(coxagent_application::ChatMsg {
        id: "msg-1".to_owned(),
        at: "2026-08-31T09:41:00Z".to_owned(),
        user: "maya".to_owned(),
        body: "the payment hook keeps timing out".to_owned(),
        edited: None,
        channel: GENERAL_CHANNEL.to_owned(),
        attachments: Vec::new(),
        reactions: Vec::new(),
        thread_id: None,
        reply_count: 0,
        deleted: false,
    });
    s
}

async fn get(client: &reqwest::Client, url: &str, bearer: Option<&str>) -> reqwest::Response {
    let mut req = client.get(url);
    if let Some(token) = bearer {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    req.send().await.unwrap()
}

#[tokio::test]
async fn search_endpoint_gates_membership_and_serves_the_hit_shape() {
    let _dir = boot(
        PORT,
        vec![handle("demo", seeded_state())],
        Some(Arc::new(StubAuth)),
    )
    .await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{PORT}/api/search");

    // No credentials: auth_mw refuses before the handler is reached.
    let resp = get(&client, &format!("{base}?q=payment"), None).await;
    assert_eq!(resp.status().as_u16(), 401, "anonymous search is refused");
    // A bearer the auth store does not know is refused the same way.
    let resp = get(
        &client,
        &format!("{base}?q=payment"),
        Some("bearer-unknown"),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401, "unknown bearer is refused");

    // A member of `demo` searches their project: 200, bare array, full shape.
    let resp = get(
        &client,
        &format!("{base}?q=payment&pid=demo"),
        Some(MEMBER_BEARER),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let hits: serde_json::Value = resp.json().await.unwrap();
    let hits = hits.as_array().expect("bare array (SA contract)");
    assert!(
        !hits.is_empty(),
        "the seeded needle matches all three kinds"
    );
    let kinds: Vec<&str> = hits.iter().map(|h| h["kind"].as_str().unwrap()).collect();
    assert!(
        kinds.contains(&"ticket") && kinds.contains(&"page") && kinds.contains(&"message"),
        "all three kinds surface: {kinds:?}"
    );
    for hit in hits {
        for field in ["kind", "id", "ref", "label", "snippet", "sub", "at", "link"] {
            assert!(
                hit.get(field).is_some(),
                "hit shape carries `{field}`: {hit}"
            );
        }
    }

    // AC2 on the wire: the same member asking for a project they are NOT a
    // member of gets the per-project routes' 403 — the handler enforces what
    // auth_mw cannot see (the pid rides in the query string).
    let resp = get(
        &client,
        &format!("{base}?q=payment&pid=secret"),
        Some(MEMBER_BEARER),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 403, "non-member pid is refused");

    // No pid: the sweep covers the projects the caller may view (demo only).
    let resp = get(&client, &format!("{base}?q=payment"), Some(MEMBER_BEARER)).await;
    assert_eq!(resp.status().as_u16(), 200);
    let swept: serde_json::Value = resp.json().await.unwrap();
    assert!(
        !swept.as_array().unwrap().is_empty(),
        "the member's own project is swept"
    );

    // AC4 on the wire: a sub-2-character query is an explicit empty array.
    let resp = get(
        &client,
        &format!("{base}?q=p&pid=demo"),
        Some(MEMBER_BEARER),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    assert!(resp
        .json::<serde_json::Value>()
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());

    // The route is registered in the OpenAPI document (CXA-C005 drift guard).
    let doc: serde_json::Value = client
        .get(format!("http://127.0.0.1:{PORT}/api/openapi.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        doc["paths"]["/api/search"]["get"].is_object(),
        "/api/search must be registered in the OpenAPI spec"
    );
}

/// Open mode (no accounts configured): the single local operator searches
/// without presenting credentials — same trust level as every other endpoint
/// on an auth-less hub.
#[tokio::test]
async fn search_endpoint_runs_open_when_no_auth_is_configured() {
    let _dir = boot(OPEN_PORT, vec![handle("demo", seeded_state())], None).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "http://127.0.0.1:{OPEN_PORT}/api/search?q=payment&pid=demo"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let hits: serde_json::Value = resp.json().await.unwrap();
    assert!(
        !hits.as_array().unwrap().is_empty(),
        "open mode sees the seeded project"
    );
}
