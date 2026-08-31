//! CXA-B129 regression: `POST /api/projects` maps the injected factory's
//! failure CLASS to the right status — an expected client conflict (the target
//! workspace already holds tickets, e.g. after delete-then-recreate against a
//! surviving store row) is 409 with the same JSON error shape, never a 500.
//! Pure in-process: requests answered via `tower::ServiceExt::oneshot`, no
//! hub process, no host harness, no TCP port.

use super::*;
use axum::body::Body;
use coxagent_infrastructure::MemoryAuditSink;
use std::sync::Arc;
use store_rpc_test_support::{CountingStore, PID, UnusedEngine};
use tower::ServiceExt;

/// A hub with no registered projects and the given factory; the exact shape
/// `create_project` needs (empty space registry → no space required).
async fn hub_with_factory(factory: ProjectFactory) -> AppState {
    let dir = tempfile::tempdir().expect("tempdir");
    build_state(
        Vec::new(),
        Arc::new(MemoryAuditSink::default()),
        HubExtras {
            factory: Some(factory),
            hub_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
}

/// A factory stub that answers every create with the same classified result.
fn factory_returning(result: Result<ProjectHandle, FactoryError>) -> ProjectFactory {
    Arc::new(move |_req| {
        let result = result.clone();
        Box::pin(async move { result })
    })
}

async fn post_create(state: AppState) -> axum::response::Response {
    Router::new()
        .route("/api/projects", post(super::projects::create_project))
        .with_state(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"QA-B126-Conflict"}"#))
                .expect("well-formed request"),
        )
        .await
        .expect("in-process request")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("json body")
}

/// The repro class: the derived id collides with a store row that survived a
/// delete (companion cleanup bug), so greenfield refuses to re-onboard. That
/// is the CLIENT asking for something already true — 409, not 500.
#[tokio::test]
async fn a_workspace_conflict_is_mapped_to_409_with_the_error_json() {
    let app = hub_with_factory(factory_returning(Err(FactoryError::conflict(
        "workspace already has tickets; refusing to re-onboard",
    ))))
    .await;

    let resp = post_create(app).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"],
        "workspace already has tickets; refusing to re-onboard",
        "the operator-facing message must survive the status mapping"
    );
}

/// Any other factory failure stays a genuine server fault: 500.
#[tokio::test]
async fn an_unclassified_factory_failure_stays_a_500() {
    let app = hub_with_factory(factory_returning(Err(FactoryError::internal(
        "store unreachable",
    ))))
    .await;

    let resp = post_create(app).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// Sanity pin on the harness: the route under test is the real one, and a
/// successful factory still lands `{"ok":true,"id":...}`.
#[tokio::test]
async fn a_successful_factory_still_returns_200_with_the_id() {
    let app = hub_with_factory(factory_returning(Ok(ProjectHandle {
        id: PID.to_owned(),
        name: "QA-B126-Conflict".to_owned(),
        alias: "QABC".to_owned(),
        store: Arc::new(CountingStore::seeded(
            coxagent_application::state::ProjectState::default(),
        )),
        runner: Arc::new(RunnerHandle::default()),
        config_path: std::path::PathBuf::from("/tmp/qa/coxagent.json"),
        engine: Arc::new(UnusedEngine),
        work_dir: std::path::PathBuf::from("/tmp/qa"),
        outbox: None,
        budget: Arc::new(std::sync::Mutex::new(
            coxagent_application::config::BudgetCaps::default(),
        )),
        context_path: std::path::PathBuf::from("/tmp/qa/project_context.md"),
        forge: None,
        deploy: None,
        storage: None,
        files: None,
        deps_discovery: None,
    })))
    .await;

    let resp = post_create(app).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["ok"], serde_json::json!(true));
    assert_eq!(body["id"], PID);
}
