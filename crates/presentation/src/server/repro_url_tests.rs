// CXA-F243 tests — the live reproduction URL for the human verify gate: the
// contract body, the honest `none` folds, and the P5a auth posture.
//
// Pure in-process verification (harness in `store_rpc_test_support`):
// requests drive the real handler, answered via `tower::ServiceExt::oneshot`.
// No hub process, no TCP port. The handler alone is exercised (no `auth_mw`
// layer) so the IN-HANDLER gate is what stands — the same seam the store
// guard tests pin.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]
use super::*;
use axum::body::Body;
use coxagent_application::state::ProjectState;
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};
use std::sync::Arc;
use store_rpc_test_support::{app_with, body_text, CountingStore, StubAuth, INSIDE_SESSION, PID};
use tower::ServiceExt;

/// A bug in `Fixed` — the verify-pending state the endpoint resolves for,
/// reached through the aggregate's own guarded transitions.
fn fixed_bug_state() -> ProjectState {
    let mut t = Ticket::new(
        TicketId::new("CXA-B001").expect("valid id"),
        TicketType::Bug,
        "crash on save",
        "the app crashes when saving an empty record",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    t.claim(Role::DevBug, "dev@host", "2026-08-20T00:00:00Z")
        .expect("unclaimed bug is claimable");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("dev may mark a claimed bug fixed");
    ProjectState {
        tickets: vec![t],
        ..ProjectState::default()
    }
}

/// The handler alone — the seam whose in-handler auth gate must hold even if
/// the route ever moves out from under `auth_mw` (P5a defense-in-depth).
fn repro_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/projects/:pid/ticket/:id/reproduction-url",
            get(ticket_reproduction_url_ep),
        )
        .with_state(state)
}

/// GET the reproduction URL, optionally carrying a session cookie.
async fn get_repro(
    router: Router,
    pid: &str,
    ticket: &str,
    session: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder().uri(format!(
        "/api/projects/{pid}/ticket/{ticket}/reproduction-url"
    ));
    if let Some(token) = session {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={token}"));
    }
    router
        .oneshot(builder.body(Body::empty()).expect("well-formed request"))
        .await
        .expect("in-process request")
}

/// Open-mode hub (no auth configured): the reviewer's read stays open. With
/// no deploy adapter and an unpublishable config, the contract body is the
/// honest `null`/`none` — never a fabricated link.
#[tokio::test]
async fn an_open_mode_hub_answers_the_contract_body_for_a_fixed_bug() {
    let state = app_with(None, Arc::new(CountingStore::seeded(fixed_bug_state()))).await;

    let resp = get_repro(repro_router(state), PID, "CXA-B001", None).await;

    assert_eq!(resp.status(), StatusCode::OK);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc, serde_json::json!({ "url": null, "reason": "none" }));
}

/// P5a posture: with auth enabled, an anonymous caller is refused by the
/// handler itself — not only by whatever middleware happens to wrap it.
#[tokio::test]
async fn an_anonymous_caller_is_refused_when_auth_is_enabled() {
    let state = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(fixed_bug_state())),
    )
    .await;

    let resp = get_repro(repro_router(state), PID, "CXA-B001", None).await;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// A signed-in principal passes the in-handler gate: the surface is
/// read-only and team-visible, like the inbox that offers the verify
/// decision itself — any role may resolve it, only anonymity is refused.
#[tokio::test]
async fn a_signed_in_principal_passes_the_gate() {
    let state = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(fixed_bug_state())),
    )
    .await;

    let resp = get_repro(repro_router(state), PID, "CXA-B001", Some(INSIDE_SESSION)).await;

    assert_eq!(resp.status(), StatusCode::OK);
}

/// An unknown ticket is a plain 404 — no probe ran, no URL was guessed.
#[tokio::test]
async fn an_unknown_ticket_answers_404() {
    let state = app_with(
        None,
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;

    let resp = get_repro(repro_router(state), PID, "CXA-B999", None).await;

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
