// CXA-F029 guard tests — `POST /api/projects/:pid/store`, the REST state-store
// endpoint a fronted runner uses instead of direct Postgres/Redis: AUTH.
//
// Pure in-process verification (harness in `store_rpc_test_support`): requests
// drive the real handler (and, where noted, the real `auth_mw` layering from
// `serve_full`) mounted on a one-route `Router`, answered via
// `tower::ServiceExt::oneshot` — the same style as `cors_rate_limit_tests`.
//
// Encoded acceptance criteria (CXA-F029):
// - AC1  auth configured: every op answers 401 with a sign-in prompt when no
//        valid principal/session/bearer is presented, and never mutates state.
// - AC2  auth configured: an invalid/expired bearer, or a session whose user
//        is not a member of :pid (even though authenticated globally), is
//        rejected instead of being forwarded to the store adapter.
// - AC3  auth NOT configured (open mode): every op keeps working with no
//        principal, exactly as before.
//
// The stale-write (revision / 409) contract lives in
// `store_rpc_stale_write_tests.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]
use super::*;
use coxagent_application::state::ProjectState;
use coxagent_infrastructure::JsonStateStore;
use std::sync::Arc;
use store_rpc_test_support::{
    all_ops, app_with, body_text, deployed_router, handler_router, post_store, CountingStore,
    StubAuth, LEAD_ELSEWHERE_SESSION, OUTSIDE_SESSION,
};

// ---------------------------------------------------------------------------
// AC1 — auth configured: every op needs a principal; nothing mutates.
// ---------------------------------------------------------------------------

/// The deployed route (handler behind `auth_mw`) must answer 401 for every
/// operation when no credentials are presented, and the store adapter must
/// never be reached.
#[tokio::test]
async fn ac1_deployed_route_answers_401_for_every_op_without_credentials() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = deployed_router(app);

    for (op, args) in all_ops() {
        let resp = post_store(router.clone(), op, args, None, None).await;
        let status = resp.status();
        let body = body_text(resp).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "unauthenticated /store?op={op} must be refused — got: {body}"
        );
    }
    assert_eq!(
        store.calls(),
        0,
        "a refused request must never reach the store adapter"
    );
}

/// The handler's own gate must carry the sign-in prompt ("sign in first") and
/// likewise forward nothing — the defense-in-depth half of AC1.
#[tokio::test]
async fn ac1_handler_gate_answers_401_sign_in_prompt_and_forwards_nothing() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = handler_router(app);

    for (op, _args) in all_ops() {
        let resp = post_store(router.clone(), op, serde_json::json!({}), None, None).await;
        let status = resp.status();
        let body = body_text(resp).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "unauthenticated /store?op={op} must be refused by the handler itself"
        );
        assert_eq!(
            body, "sign in first",
            "the refusal must carry the sign-in prompt"
        );
    }
    assert_eq!(store.calls(), 0);
}

// ---------------------------------------------------------------------------
// AC2 — auth configured: invalid/expired bearer or an out-of-project session
// is rejected instead of forwarded to the store adapter.
// ---------------------------------------------------------------------------

/// A bearer token the auth store does not recognize (invalid or expired) must
/// be treated as anonymous: 401 on the deployed route, adapter untouched.
#[tokio::test]
async fn ac2_invalid_or_expired_bearer_never_reaches_the_store() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = deployed_router(app);

    for bearer in ["Bearer garbage", "Bearer bearer-expired"] {
        for (op, args) in all_ops() {
            let resp = post_store(router.clone(), op, args, Some(bearer), None).await;
            let status = resp.status();
            let body = body_text(resp).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{bearer} on /store?op={op} must be refused — got: {body}"
            );
        }
    }
    assert_eq!(store.calls(), 0);
}

/// A session whose user is NOT a member of :pid — even though authenticated
/// globally — must be rejected by /store itself instead of being forwarded to
/// the store adapter. This is the in-handler half of RBAC scoping: the gate
/// that keeps the adapter closed when /store is reached by any path that is
/// not the middleware-wired route (the historical-defect simulation). Two
/// personas, two distinct gate branches, each pinned to its exact refusal:
/// - member-tier outsider ([`OUTSIDE_SESSION`]) trips the manage bar first
///   (403 "management role required");
/// - manage-tier outsider ([`LEAD_ELSEWHERE_SESSION`]) clears the manage bar
///   and must then be stopped by the project-membership branch (403 "not a
///   member of this project").
#[tokio::test]
async fn ac2_store_rejects_an_outside_session_instead_of_forwarding_it() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = handler_router(app);

    let personas = [
        (
            OUTSIDE_SESSION,
            StatusCode::FORBIDDEN,
            "management role required",
        ),
        (
            LEAD_ELSEWHERE_SESSION,
            StatusCode::FORBIDDEN,
            "not a member of this project",
        ),
    ];
    for (session, expected, reason) in personas {
        for (op, args) in all_ops() {
            let resp = post_store(router.clone(), op, args, None, Some(session)).await;
            let status = resp.status();
            let body = body_text(resp).await;
            assert_eq!(
                status, expected,
                "/store must refuse {session} on op={op} ({reason}) — got: {body}"
            );
            assert!(
                body.contains(reason),
                "the refusal must name why ({reason}) — got: {body}"
            );
        }
    }
    assert_eq!(
        store.calls(),
        0,
        "the store adapter must never be reached for a non-member session"
    );
}

/// Cross-project RBAC scoping on the deployed route: `auth_mw` refuses a
/// globally-authenticated user who is not a member of :pid before the handler
/// ever runs.
#[tokio::test]
async fn ac2_deployed_route_scopes_a_global_session_to_project_membership() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = deployed_router(app);

    for (op, args) in all_ops() {
        let resp = post_store(router.clone(), op, args, None, Some(OUTSIDE_SESSION)).await;
        let status = resp.status();
        let body = body_text(resp).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a user outside :pid must be scoped away from /store?op={op} — got: {body}"
        );
    }
    assert_eq!(store.calls(), 0);
}

// ---------------------------------------------------------------------------
// AC3 — open mode (no auth configured): every op keeps working unchanged.
// ---------------------------------------------------------------------------

/// With no AuthPort wired, every operation must execute with no principal at
/// all — the unauthenticated operators this endpoint has always served. Runs
/// against the real `JsonStateStore` (the hub's own default adapter), not a
/// stub, so "behave exactly as before" is observed on the real backend.
#[tokio::test]
async fn ac3_open_mode_executes_every_op_without_a_principal() {
    let state_dir = tempfile::tempdir().expect("state dir");
    let store: Arc<dyn StateStorePort> =
        Arc::new(JsonStateStore::new(state_dir.path().join("state")).expect("json store"));
    let app = app_with(None, store).await;
    let router = deployed_router(app);

    for (op, args) in all_ops() {
        let resp = post_store(router.clone(), op, args, None, None).await;
        let status = resp.status();
        let body = body_text(resp).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "open-mode /store?op={op} must keep working without a principal — got: {body}"
        );
    }
}
