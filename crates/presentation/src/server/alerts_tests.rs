// CXA-F235 tests — the outbound alert delivery-history surface:
//
//   * `GET /api/projects/:pid/alerts` — newest-first entries with the derived
//     `retrying` flag (a pending entry with failed attempts reads as "failed,
//     retrying", not a first send in flight).
//   * `POST /api/projects/:pid/alerts/:id/replay` — requeues only a dead
//     alert; pending/delivered/unknown ids are refused with 404, a
//     non-numeric id with 400.
//
// Pure in-process verification (harness in `store_rpc_test_support`): requests
// drive the real handlers via `tower::ServiceExt::oneshot`; the spool is a
// real `MemoryOutboxStore` (the same port the dashboard binds), so no hub
// process, no TCP port.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]
use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use coxagent_application::ports::outbound::{MemoryOutboxStore, OutboxStorePort};
use coxagent_application::state::{OutboxEntry, OutboxStatus};
use coxagent_infrastructure::JsonStateStore;
use std::sync::Arc;
use store_rpc_test_support::{app_with, body_text, PID};
use tower::ServiceExt;

/// A state store over a throwaway dir — the harness fixture's store, unused by
/// the alerts surface itself.
fn throwaway_store() -> Arc<JsonStateStore> {
    Arc::new(JsonStateStore::new(tempfile::tempdir().expect("tempdir").keep()).expect("store"))
}

/// A router with only the alerts surface, the handler seam (the deployed
/// router adds `auth_mw`, which is where these routes' auth lives — see
/// `serve_full`; the handlers themselves carry no auth, like `audit_ep`).
fn alerts_router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/api/projects/:pid/alerts", get(list_alerts_ep))
        .route(
            "/api/projects/:pid/alerts/:id/replay",
            post(replay_alert_ep),
        )
        .with_state(state)
}

/// A hub whose project's outbox is `spool`, so tests drive the real port.
async fn app_with_spool(spool: Arc<MemoryOutboxStore>) -> AppState {
    let state = app_with(None, throwaway_store()).await;
    {
        let mut projects = state.projects.write().await;
        if let Some(handle) = projects.get_mut(PID) {
            handle.outbox = Some(spool);
        }
    }
    state
}

async fn get_alerts(state: AppState, pid: &str) -> axum::response::Response {
    alerts_router(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/projects/{pid}/alerts"))
                .body(Body::empty())
                .expect("well-formed request"),
        )
        .await
        .expect("in-process request")
}

async fn replay(state: AppState, pid: &str, id: &str) -> axum::response::Response {
    alerts_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{pid}/alerts/{id}/replay"))
                .body(Body::empty())
                .expect("well-formed request"),
        )
        .await
        .expect("in-process request")
}

/// Seed the spool with one delivered entry and one that failed twice.
async fn seeded_spool() -> (Arc<MemoryOutboxStore>, u64, u64) {
    let spool = Arc::new(MemoryOutboxStore::new());
    spool
        .enqueue(OutboxEntry::pending("deploy_ok", "demo", "shipped", 1_000))
        .await;
    let delivered = spool.recent(10).await[0].id;
    spool.mark_delivered(delivered).await;

    spool
        .enqueue(OutboxEntry::pending("deploy_failed", "demo", "boom", 2_000))
        .await;
    let retrying = spool.recent(10).await[0].id;
    spool.mark_retry(retrying, 9_999).await;
    spool.mark_retry(retrying, 9_999).await;
    (spool, delivered, retrying)
}

#[tokio::test]
async fn list_is_newest_first_with_the_retrying_flag_derived() {
    let (spool, delivered, retrying) = seeded_spool().await;
    let state = app_with_spool(spool).await;

    let resp = get_alerts(state, PID).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rows: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json rows");
    assert_eq!(rows.as_array().expect("rows").len(), 2);
    assert_eq!(rows[0]["id"], retrying, "newest first");
    assert_eq!(rows[0]["kind"], "deploy_failed");
    assert_eq!(
        rows[0]["retrying"], true,
        "pending with attempts reads as retrying"
    );
    assert_eq!(rows[0]["status"], "pending");
    assert_eq!(rows[0]["attempts"], 2);
    assert_eq!(rows[1]["id"], delivered);
    assert_eq!(rows[1]["status"], "delivered");
    assert_eq!(rows[1]["retrying"], false);
}

#[tokio::test]
async fn a_project_without_a_spool_lists_as_empty_not_an_error() {
    let state = app_with(None, throwaway_store()).await;
    let resp = get_alerts(state, PID).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_text(resp).await.trim(), "[]");
}

#[tokio::test]
async fn unknown_project_is_404() {
    let (spool, _, _) = seeded_spool().await;
    let state = app_with_spool(spool).await;
    let resp = get_alerts(state, "no-such-project").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn replay_requeues_a_dead_alert_and_refuses_everything_else() {
    let (spool, delivered, retrying) = seeded_spool().await;
    spool.mark_dead(retrying).await;
    let state = app_with_spool(spool.clone()).await;

    // Dead → replayed: the entry is pending again with a clean attempt count.
    let resp = replay(state.clone(), PID, &retrying.to_string()).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let replayed_entry = &spool.recent(10).await[0];
    assert_eq!(replayed_entry.status, OutboxStatus::Pending);
    assert_eq!(replayed_entry.attempts, 0);

    // Already-pending, delivered and unknown ids are not replayable.
    assert_eq!(
        replay(state.clone(), PID, &retrying.to_string())
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        replay(state.clone(), PID, &delivered.to_string())
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        replay(state.clone(), PID, "99999").await.status(),
        StatusCode::NOT_FOUND
    );
    // A non-numeric id is a bad request, not a lookup miss.
    assert_eq!(
        replay(state, PID, "not-a-number").await.status(),
        StatusCode::BAD_REQUEST
    );
}
