// In-process verification of the archive read-back (CXA-F274), on the shared
// store_rpc_test_support harness: requests answered via `tower::ServiceExt::
// oneshot`, the real `auth_mw` layering exactly as `serve_full` mounts it, and
// ArchiveStorePort doubles — the real in-memory adapter plus a failing one.
// Pure: no server, no host harness, no TCP port.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]

use super::store_rpc_test_support::{
    app_with, body_text, CountingStore, StubAuth, INSIDE_SESSION, PID,
};
use super::*;
use axum::body::Body;
use coxagent_application::error::PortError;
use coxagent_application::ports::outbound::ArchiveStorePort;
use coxagent_application::state::{Evidence, ProjectState, TicketAttachment};
use coxagent_domain::ticket::Ticket;
use coxagent_domain::{Complexity, Priority, Role, TechnicalDesign, TicketId, TicketType};
use coxagent_infrastructure::MemoryArchiveStore;
use tower::ServiceExt;

/// A cold store that is DOWN: every read faults, so a test can prove the
/// endpoints answer 500 — never a silent empty or a silent 404.
struct FailingArchive;

#[async_trait::async_trait]
impl ArchiveStorePort for FailingArchive {
    async fn put(&self, _project: &str, _ticket: &Ticket) -> Result<(), PortError> {
        unreachable!("the read-back never writes")
    }
    async fn get(&self, _project: &str, _id: &str) -> Result<Option<Ticket>, PortError> {
        Err(PortError::Backend("cold store down".to_owned()))
    }
    async fn list(&self, _project: &str) -> Result<Vec<Ticket>, PortError> {
        Err(PortError::Backend("cold store down".to_owned()))
    }
}

fn ticket(id: &str) -> Ticket {
    Ticket::new(
        TicketId::new(id).expect("valid id"),
        TicketType::Feature,
        format!("ticket {id}"),
        "archived work",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket")
}

/// An archived ticket carrying exactly what the detail dialog must still
/// render after eviction: the SA design specs, the acceptance-criteria
/// checklist, and a test case with its verdict.
fn designed_archived_ticket(id: &str) -> Ticket {
    let mut t = ticket(id);
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            approach: "cold store behind the port".to_owned(),
            files: vec!["crates/application/src/ports/outbound/archive.rs".to_owned()],
            ..TechnicalDesign::default()
        },
    )
    .expect("SA owns the design");
    t.set_acceptance_criteria(vec!["archived work still opens by id".to_owned()]);
    t
}

/// A hot state holding one LIVE ticket plus the joins the archived one keeps
/// (comments/evidence/attachments stay hot-side, keyed by id — F273).
fn hot_state() -> ProjectState {
    let mut s = ProjectState::default();
    s.tickets.push(ticket("CXC-HOT"));
    s.ticket_evidence.insert(
        "CXC-ARCH".to_owned(),
        vec![Evidence {
            kind: "test".to_owned(),
            label: "archive read-back spec".to_owned(),
            detail: "cargo test archive_read".to_owned(),
            at: "2026-09-01T00:00:00Z".to_owned(),
            source_gates: Vec::new(),
            actor: "TEST".to_owned(),
        }],
    );
    s.ticket_attachments.insert(
        "CXC-ARCH".to_owned(),
        vec![TicketAttachment {
            name: "cold-store.png".to_owned(),
            key: "proj/demo/cold-store.png".to_owned(),
            content_type: "image/png".to_owned(),
            by: "PD".to_owned(),
            at: "2026-09-01T00:00:00Z".to_owned(),
        }],
    );
    s
}

/// The harness app with an archive store wired beside the hot store.
async fn app_with_archive(store: ProjectState, archive: Arc<dyn ArchiveStorePort>) -> AppState {
    let mut state = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(store)),
    )
    .await;
    state.archive_store = Some(archive);
    state
}

/// The two read-back routes behind `auth_mw`, exactly as `serve_full` layers
/// them (route_layer + with_state).
fn archive_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/projects/:pid/tickets/archive",
            get(super::archive::ticket_archive_ep),
        )
        .route(
            "/api/projects/:pid/ticket/:id",
            get(super::work::ticket_detail_ep),
        )
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state)
}

async fn get_at(router: Router, path: &str, session: Option<&str>) -> axum::response::Response {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(token) = session {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={token}"));
    }
    router
        .oneshot(builder.body(Body::empty()).expect("well-formed request"))
        .await
        .expect("in-process request")
}

async fn seeded_archive(ids: &[&str]) -> MemoryArchiveStore {
    let mem = MemoryArchiveStore::new();
    for id in ids {
        mem.put(PID, &ticket(id)).await.expect("seed archive");
    }
    mem
}

#[tokio::test]
async fn the_archive_lists_seeded_tickets_newest_first_with_the_archived_stamp() {
    let app = app_with_archive(
        hot_state(),
        Arc::new(seeded_archive(&["CXC-F001", "CXC-F010", "CXC-B002"]).await),
    )
    .await;
    let resp = get_at(
        archive_router(app),
        "/api/projects/demo/tickets/archive",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    let ids: Vec<&str> = doc["tickets"]
        .as_array()
        .expect("tickets array")
        .iter()
        .map(|t| t["id"].as_str().expect("ticket id"))
        .collect();
    assert_eq!(ids, ["CXC-F010", "CXC-F001", "CXC-B002"], "id-descending");
    for t in doc["tickets"].as_array().unwrap() {
        assert_eq!(t["archived"], serde_json::json!(true), "stamped archived");
        assert!(t.get("design").is_none(), "list shape strips design specs");
    }
    assert_eq!(doc["total"], serde_json::json!(3));
}

#[tokio::test]
async fn the_archive_serves_paged_windows_with_the_full_total() {
    let app = app_with_archive(
        ProjectState::default(),
        Arc::new(
            seeded_archive(&["CXC-F001", "CXC-F002", "CXC-F003", "CXC-F004", "CXC-F005"]).await,
        ),
    )
    .await;
    let router = archive_router(app.clone());
    let resp = get_at(
        router.clone(),
        "/api/projects/demo/tickets/archive?limit=2&offset=1",
        Some(INSIDE_SESSION),
    )
    .await;
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    let ids: Vec<&str> = doc["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["CXC-F004", "CXC-F003"]);
    assert_eq!(doc["total"], serde_json::json!(5), "total spans pages");

    // The clamps: a nonsense limit answers one row; an oversized one caps.
    let resp = get_at(
        router,
        "/api/projects/demo/tickets/archive?limit=0",
        Some(INSIDE_SESSION),
    )
    .await;
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["tickets"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_unknown_project_is_a_404_on_the_archive_too() {
    let app = app_with_archive(ProjectState::default(), Arc::new(MemoryArchiveStore::new())).await;
    let resp = get_at(
        archive_router(app),
        "/api/projects/nowhere/tickets/archive",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn no_store_or_an_empty_archive_answers_the_pinned_empty_shape() {
    // Store disabled (the default local boot)…
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    let resp = get_at(
        archive_router(app),
        "/api/projects/demo/tickets/archive",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        body_text(resp).await,
        r#"{"tickets":[],"total":0}"#,
        "byte-exact empty shape: the UI and API read as before archival"
    );
    // …and a wired but empty store.
    let app = app_with_archive(ProjectState::default(), Arc::new(MemoryArchiveStore::new())).await;
    let resp = get_at(
        archive_router(app),
        "/api/projects/demo/tickets/archive",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(body_text(resp).await, r#"{"tickets":[],"total":0}"#);
}

#[tokio::test]
async fn a_cold_store_fault_on_the_listing_is_a_500_never_a_silent_empty() {
    let app = app_with_archive(ProjectState::default(), Arc::new(FailingArchive)).await;
    let resp = get_at(
        archive_router(app),
        "/api/projects/demo/tickets/archive",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn unauthenticated_archive_requests_are_refused_like_sibling_endpoints() {
    let app = app_with_archive(ProjectState::default(), Arc::new(MemoryArchiveStore::new())).await;
    let router = archive_router(app.clone());
    let resp = get_at(router, "/api/projects/demo/tickets/archive", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // A valid session for ANOTHER project is equally refused.
    let resp = get_at(
        archive_router(app),
        "/api/projects/demo/tickets/archive",
        Some("session-outside"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_hot_miss_falls_back_to_the_archive_with_the_full_record_and_joins() {
    let mem = MemoryArchiveStore::new();
    mem.put(PID, &designed_archived_ticket("CXC-ARCH"))
        .await
        .expect("seed archive");
    let app = app_with_archive(hot_state(), Arc::new(mem)).await;
    let resp = get_at(
        archive_router(app),
        "/api/projects/demo/ticket/CXC-ARCH",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["archived"], serde_json::json!(true), "stamped archived");
    assert_eq!(
        doc["design"]["technical"]["approach"],
        serde_json::json!("cold store behind the port"),
        "the full record, design specs included"
    );
    assert_eq!(
        doc["acceptance_criteria"],
        serde_json::json!(["archived work still opens by id"])
    );
    assert_eq!(
        doc["evidence"][0]["label"],
        serde_json::json!("archive read-back spec"),
        "hot-side joins still ride the payload when present"
    );
    assert_eq!(
        doc["attachments"][0]["name"],
        serde_json::json!("cold-store.png")
    );
}

#[tokio::test]
async fn a_hot_hit_never_consults_the_archive_and_carries_no_stamp() {
    // The cold store is DOWN on purpose: a 200 proves the hot path never
    // reached it.
    let app = app_with_archive(hot_state(), Arc::new(FailingArchive)).await;
    let resp = get_at(
        archive_router(app),
        "/api/projects/demo/ticket/CXC-HOT",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert!(doc.get("archived").is_none(), "hot tickets are not stamped");
}

#[tokio::test]
async fn a_miss_on_both_stores_is_the_unchanged_404() {
    let app = app_with_archive(hot_state(), Arc::new(MemoryArchiveStore::new())).await;
    let resp = get_at(
        archive_router(app),
        "/api/projects/demo/ticket/CXC-NOPE",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_text(resp).await, "no such project", "byte-identical");
}

#[tokio::test]
async fn a_cold_store_fault_on_the_detail_fallback_is_a_500_never_a_404() {
    let app = app_with_archive(hot_state(), Arc::new(FailingArchive)).await;
    let resp = get_at(
        archive_router(app),
        "/api/projects/demo/ticket/CXC-ARCH-MISSING",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
