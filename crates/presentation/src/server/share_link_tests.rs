// CXA-F069 tests — public share-link status pages: the admin mint/list/revoke
// surface and the unauthenticated `/s/:token` page contract.
//
// Pure in-process verification (harness in `store_rpc_test_support`): requests
// drive the real handlers behind the real `auth_mw` layering from `serve_full`,
// answered via `tower::ServiceExt::oneshot`. No hub process, no TCP port.
//
// Encoded acceptance criteria (CXA-F069):
// - AC1  an admin mints an unguessable token, the URL renders for an
//        UNAUTHENTICATED caller, and revoking makes the same URL 404 on the
//        very next fetch.
// - AC2  the page shows only safe aggregates: project identity plus counts,
//        ids and sanitized summaries — never spend, design estimates, audit
//        content, transcripts, or the token itself in the HTML source.
// - AC3  management is admin-gated: a member-tier project member is refused
//        by the handler gate; a manage-tier NON-member is refused earlier by
//        the per-project membership gate.
// - AC4  unknown tokens, revoked tokens and unknown project ids answer 404
//        without distinguishing between them.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]
use super::*;
use axum::body::Body;
use coxagent_application::state::ProjectState;
use std::sync::Arc;
use store_rpc_test_support::{
    app_with, body_text, CountingStore, StubAuth, INSIDE_BEARER, LEAD_ELSEWHERE_SESSION,
    MEMBER_SESSION, OTHER_PID, PID,
};
use tower::ServiceExt;

/// The deployed shape of the share-link surface: handlers behind `auth_mw`,
/// exactly as `serve_full` layers them.
fn share_router(state: AppState) -> Router {
    Router::new()
        .route("/s/:token", get(share_page_ep))
        .route(
            "/api/projects/:pid/share-links",
            get(share_link_list_ep).post(share_link_create_ep),
        )
        .route(
            "/api/projects/:pid/share-links/:token",
            axum::routing::delete(share_link_revoke_ep),
        )
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state)
}

/// Fire one request with an optional `Authorization: Bearer` header, an
/// optional session cookie, and an optional JSON body.
async fn request(
    router: Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    session: Option<&str>,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(b) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {b}"));
    }
    if let Some(s) = session {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={s}"));
    }
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    router
        .oneshot(
            builder
                .body(match body {
                    Some(v) => Body::from(v.to_string()),
                    None => Body::empty(),
                })
                .expect("well-formed request"),
        )
        .await
        .expect("in-memory request")
}

/// A project state whose ONLY safe export is the project name — every other
/// fixture value must stay off the public page.
fn seeded_state() -> ProjectState {
    ProjectState {
        spend: coxagent_application::state::Spend {
            total_cost_usd: 123.45,
            runs: 9,
            ..Default::default()
        },
        ..ProjectState::default()
    }
}

/// Mint one share link as the admin and return its token.
async fn mint_link(router: &Router) -> String {
    let resp = request(
        router.clone(),
        "POST",
        &format!("/api/projects/{PID}/share-links"),
        Some(INSIDE_BEARER),
        None,
        Some(serde_json::json!({ "name": "ACME stakeholder" })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "admin mint must succeed");
    let created: serde_json::Value =
        serde_json::from_str(&body_text(resp).await).expect("json body");
    let token = created["token"].as_str().expect("token string").to_owned();
    assert_eq!(token.len(), 32, "unguessable CSPRNG token (32 hex chars)");
    assert_eq!(created["name"], "ACME stakeholder", "the label round-trips");
    token
}

#[tokio::test]
async fn ac1_admin_mints_and_the_public_page_renders() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(seeded_state())),
    )
    .await;
    let router = share_router(app);
    let token = mint_link(&router).await;

    // The page renders for a caller with NO credentials at all.
    let resp = request(
        router.clone(),
        "GET",
        &format!("/s/{token}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "the token is the credential");
    // Hardening headers on a PUBLIC page: strict CSP (the page is
    // script-free), no caching of a secret URL, no framing, no sniffing, and
    // a strict referrer policy.
    let csp = resp
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"),
        "the public page must ship a deny-by-default CSP, got: {csp}"
    );
    assert_eq!(
        resp.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "a secret-URL page must not sit in caches"
    );
    assert_eq!(
        resp.headers()
            .get(header::REFERRER_POLICY)
            .and_then(|v| v.to_str().ok()),
        Some("strict-origin-when-cross-origin")
    );
    let html = body_text(resp).await;
    assert!(html.contains("Demo"), "the page names the project");
    assert!(
        !html.contains(&token),
        "AC2/AC5: the token must not appear in the HTML source"
    );
    assert!(
        !html.contains("Spend") && !html.contains("$123"),
        "AC2: internal spend must not leak onto the page"
    );

    // The list shows the link as active.
    let resp = request(
        router,
        "GET",
        &format!("/api/projects/{PID}/share-links"),
        Some(INSIDE_BEARER),
        None,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json");
    assert_eq!(list[0]["token"], token.as_str());
    assert_eq!(list[0]["revoked"], serde_json::json!(false));
}

#[tokio::test]
async fn ac1_revocation_kills_the_url_on_the_next_fetch() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(seeded_state())),
    )
    .await;
    let router = share_router(app);
    let token = mint_link(&router).await;

    let resp = request(
        router.clone(),
        "DELETE",
        &format!("/api/projects/{PID}/share-links/{token}"),
        Some(INSIDE_BEARER),
        None,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = request(
        router.clone(),
        "GET",
        &format!("/s/{token}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "AC1: revocation kills the URL on the next fetch"
    );
    let resp = request(
        router,
        "GET",
        &format!("/api/projects/{PID}/share-links"),
        Some(INSIDE_BEARER),
        None,
        None,
    )
    .await;
    let list: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json");
    assert_eq!(
        list[0]["revoked"],
        serde_json::json!(true),
        "the record stays for the audit trail"
    );
}

#[tokio::test]
async fn ac3_member_tier_is_refused_and_non_member_manage_tier_hits_membership_gate() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    let router = share_router(app);

    // Carol: BE member of the project — a worker, not an administrator.
    for method in ["POST", "GET"] {
        let resp = request(
            router.clone(),
            method,
            &format!("/api/projects/{PID}/share-links"),
            None,
            Some(MEMBER_SESSION),
            method.eq("POST").then(|| serde_json::json!({})),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "member tier must not {method} share links"
        );
    }

    // Morgan: manage tier, but member of another project — auth_mw's
    // per-project membership gate refuses before the handler runs.
    let resp = request(
        router.clone(),
        "POST",
        &format!("/api/projects/{PID}/share-links"),
        None,
        Some(LEAD_ELSEWHERE_SESSION),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a manage-tier non-member still hits the membership gate"
    );
}

#[tokio::test]
async fn ac4_unknown_token_unknown_pid_and_foreign_token_all_answer_404() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    let router = share_router(app);

    let resp = request(
        router.clone(),
        "GET",
        "/s/deadbeefdeadbeefdeadbeefdeadbeef",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // A minted link belongs to ITS project only: revoking it via another
    // project's path finds nothing.
    let resp = request(
        router.clone(),
        "POST",
        &format!("/api/projects/{PID}/share-links"),
        Some(INSIDE_BEARER),
        None,
        Some(serde_json::json!({})),
    )
    .await;
    let created: serde_json::Value =
        serde_json::from_str(&body_text(resp).await).expect("json body");
    let token = created["token"].as_str().expect("token");

    let resp = request(
        router.clone(),
        "DELETE",
        &format!("/api/projects/{OTHER_PID}/share-links/{token}"),
        Some(INSIDE_BEARER),
        None,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = request(
        router.clone(),
        "DELETE",
        &format!("/api/projects/ghost/share-links/{token}"),
        Some(INSIDE_BEARER),
        None,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "unknown pid is 404");

    let resp = request(
        router.clone(),
        "DELETE",
        &format!("/api/projects/{PID}/share-links/never-minted"),
        Some(INSIDE_BEARER),
        None,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "unknown token is 404");
}

#[tokio::test]
async fn ac6_open_mode_mints_and_renders_without_any_principal() {
    let app = app_with(None, Arc::new(CountingStore::seeded(seeded_state()))).await;
    let router = share_router(app);

    let resp = request(
        router.clone(),
        "POST",
        &format!("/api/projects/{PID}/share-links"),
        None,
        None,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "open mode (no auth configured) stays open, like every other surface"
    );
    let created: serde_json::Value =
        serde_json::from_str(&body_text(resp).await).expect("json body");
    let token = created["token"].as_str().expect("token").to_owned();

    let resp = request(router, "GET", &format!("/s/{token}"), None, None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// `{name?}` is optional: a bare POST with no body at all (how a minimal
/// settings client would call it) must still mint, with an empty label.
#[tokio::test]
async fn create_accepts_a_bodyless_post() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    let router = share_router(app);

    let resp = request(
        router,
        "POST",
        &format!("/api/projects/{PID}/share-links"),
        Some(INSIDE_BEARER),
        None,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "no body is a valid mint");
    let created: serde_json::Value =
        serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(
        created["token"].as_str().map(str::len),
        Some(32),
        "a token is minted even without a body"
    );
    assert_eq!(
        created["name"],
        serde_json::json!(""),
        "name defaults to empty"
    );
}
