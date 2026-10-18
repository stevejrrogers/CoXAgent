// CXA-F031 tests — `POST /api/projects/:pid/store`, the REST state-store
// endpoint a fronted runner uses instead of direct Postgres/Redis: the P5a
// auth-enforcement contract.
//
// Pure in-process verification (harness in `store_rpc_test_support`): requests
// drive the real handler — and, where a criterion names the deployed route,
// the real `auth_mw` layering from `serve_full` — mounted on a one-route
// `Router`, answered via `tower::ServiceExt::oneshot`. No hub process, no
// host harness, no TCP port.
//
// Encoded acceptance criteria (CXA-F031):
// - AC1  auth enabled (`app.auth` is Some) and no Authorization header: every
//        operation type answers 401 Unauthorized with the body "sign in first".
// - AC2  auth enabled and an invalid/expired bearer (`resolve_principal` ->
//        None): 401 Unauthorized, and no store operation executes or mutates
//        state.
// - AC3  auth enabled and a valid principal token: every operation (load,
//        version, save, claim_ticket, acquire_leader, claim_stage,
//        release_stage, heartbeat, workers, set_desired, get_desired,
//        acquire_operator) succeeds exactly as it did before auth was added.
// - AC4  open mode (`app.auth` is None): unauthenticated requests are accepted
//        for every operation — open modes stay open, behaviour unchanged.
// - AC5  enforcement sits after project resolution but before op dispatch: an
//        unauthorized project id still answers not_found rather than leaking
//        whether the store op would run, and nothing is dispatched.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]
use super::*;
use coxagent_application::state::ProjectState;
use std::sync::Arc;
use store_rpc_test_support::{
    all_ops, app_with, body_text, deployed_router, handler_router, post_store, post_store_at,
    CountingStore, StubAuth, INSIDE_BEARER,
};

// ---------------------------------------------------------------------------
// AC1 — auth enabled, no Authorization header: 401 "sign in first" per op.
// ---------------------------------------------------------------------------

/// The endpoint's own gate must refuse every operation with the exact sign-in
/// prompt when no credentials at all are presented. Mounted on the bare
/// handler (`handler_router`) so the pinned body is the endpoint's own
/// decision, not the middleware's JSON refusal.
#[tokio::test]
async fn ac1_no_authorization_header_answers_401_sign_in_first_for_every_op() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store).await;
    let router = handler_router(app);

    for (op, _args) in all_ops() {
        let resp = post_store(router.clone(), op, serde_json::json!({}), None, None).await;
        let status = resp.status();
        let body = body_text(resp).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "unauthenticated /store?op={op} must answer 401 — got: {body}"
        );
        assert_eq!(
            body, "sign in first",
            "the 401 body must be the sign-in prompt on /store?op={op}"
        );
    }
}

// ---------------------------------------------------------------------------
// AC2 — auth enabled, invalid/expired bearer: 401, no operation executes.
// ---------------------------------------------------------------------------

/// A bearer token `resolve_principal` cannot resolve must be refused with 401
/// for every operation, and the call-counting store proves no operation —
/// read or write — ever reached (let alone mutated) the state store.
#[tokio::test]
async fn ac2_invalid_or_expired_bearer_answers_401_and_runs_no_store_operation() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = handler_router(app);

    for bearer in ["Bearer garbage", "Bearer bearer-expired"] {
        for (op, _args) in all_ops() {
            let resp = post_store(
                router.clone(),
                op,
                serde_json::json!({}),
                Some(bearer),
                None,
            )
            .await;
            let status = resp.status();
            let body = body_text(resp).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{bearer} on /store?op={op} must answer 401 — got: {body}"
            );
        }
    }
    assert_eq!(
        store.calls(),
        0,
        "a refused request must never execute a store operation"
    );
}

// ---------------------------------------------------------------------------
// AC3 — auth enabled, valid principal: every op succeeds exactly as before.
// ---------------------------------------------------------------------------

/// Alice's bearer token (manage tier, member of the project) must sail through
/// the deployed route — `auth_mw` plus the handler gate — for every operation,
/// with each op actually dispatching onto the store adapter exactly once: the
/// "exactly as it did before auth was added" contract.
#[tokio::test]
async fn ac3_valid_principal_token_executes_every_op_exactly_as_before() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = deployed_router(app);
    let bearer = format!("Bearer {INSIDE_BEARER}");
    let ops = all_ops();

    for (op, args) in &ops {
        let resp = post_store(router.clone(), op, args.clone(), Some(&bearer), None).await;
        let status = resp.status();
        let body = body_text(resp).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a valid principal on /store?op={op} must succeed — got: {body}"
        );
    }
    assert_eq!(
        store.calls(),
        ops.len(),
        "every operation must actually dispatch onto the store adapter"
    );
}

// ---------------------------------------------------------------------------
// AC4 — open mode (no auth configured): every op stays open, unchanged.
// ---------------------------------------------------------------------------

/// With no `AuthPort` wired, every operation must keep executing for a request
/// with no credentials at all — the deployed route passes straight through,
/// and the counting store proves each op really ran.
#[tokio::test]
async fn ac4_open_mode_accepts_unauthenticated_requests_for_every_op() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(None, store.clone()).await;
    let router = deployed_router(app);
    let ops = all_ops();

    for (op, args) in &ops {
        let resp = post_store(router.clone(), op, args.clone(), None, None).await;
        let status = resp.status();
        let body = body_text(resp).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "open-mode /store?op={op} must stay open without a principal — got: {body}"
        );
    }
    assert_eq!(
        store.calls(),
        ops.len(),
        "every operation must execute with no principal at all"
    );
}

// ---------------------------------------------------------------------------
// AC5 — enforcement after project resolution, before op dispatch.
// ---------------------------------------------------------------------------

/// An unauthorized project id must answer the same not_found to an
/// unauthenticated caller and to a fully-privileged one — the auth gate sits
/// AFTER project resolution, so the response cannot leak whether a store op
/// would have run — and the counting store proves nothing was ever dispatched.
/// `post_store_at` takes the project id; `ghost` is one that does not resolve.
#[tokio::test]
async fn ac5_unknown_project_answers_not_found_before_auth_and_dispatches_nothing() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = handler_router(app);
    let bearer = format!("Bearer {INSIDE_BEARER}");

    for credentials in [None, Some(bearer.as_str())] {
        for (op, _args) in all_ops() {
            let resp = post_store_at(
                router.clone(),
                "ghost",
                op,
                serde_json::json!({}),
                credentials,
                None,
            )
            .await;
            let status = resp.status();
            let body = body_text(resp).await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "unauthorized project id on /store?op={op} must answer not_found — got: {body}"
            );
        }
    }
    assert_eq!(
        store.calls(),
        0,
        "an unresolved project must dispatch no store operation"
    );
}
