// CXA-B141 tests — GET /api/projects is enumerated PER CALLER.
//
// Pure in-process verification (harness in `store_rpc_test_support`): requests
// drive the real `list_projects` handler behind the real `auth_mw` layering
// from `serve_full`, answered via `tower::ServiceExt::oneshot`. No hub
// process, no TCP port.
//
// The bug: the bare list endpoint returned the WHOLE registry to any
// signed-in account — a viewer (or any non-member) could enumerate every
// tenant's project ids, names and ticket counts, projects the same account's
// per-project reads 403 on ("not a member of this project"). The list must
// apply the SAME membership rule `auth_mw` enforces per project.
//
// Encoded acceptance criteria (CXA-B141):
// - AC1  a signed-in viewer with no memberships gets an EMPTY registry (200),
//        never another team's ids/names/counts.
// - AC2  a member-tier account sees exactly the projects it is assigned to.
// - AC3  a manage-tier lead who is not a member of a project does not see it
//        either — the list cannot name what the per-project gate would 403.
// - AC4  Super/Admin see the full registry (their per-project reads bypass).
// - AC5  broken registrations obey the same rule (id + config path + reason
//        are not other teams' business).
// - AC6  open mode (no auth configured) still lists everything — the local
//        single-operator behaviour the dashboard depends on.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]
use super::*;
use axum::body::Body;
use coxagent_application::state::ProjectState;
use std::sync::Arc;
use store_rpc_test_support::{
    app_with, body_text, CountingStore, StubAuth, INSIDE_BEARER, INSIDE_SESSION,
    LEAD_ELSEWHERE_SESSION, MEMBER_SESSION, OTHER_PID, OUTSIDE_SESSION, PID,
    VIEWER_NOWHERE_SESSION,
};
use tower::ServiceExt;

/// The deployed shape of the list surface: the handler behind `auth_mw`,
/// exactly as `serve_full` layers it.
fn list_router(state: AppState) -> Router {
    Router::new()
        .route("/api/projects", get(list_projects))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state)
}

/// GET /api/projects with an optional `Authorization: Bearer` header and an
/// optional session cookie — the two credential transports `resolve_principal`
/// accepts (bearer wins, the shape the REST store runner authenticates with).
async fn get_projects(
    router: Router,
    bearer: Option<&str>,
    session: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder().method("GET").uri("/api/projects");
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(s) = session {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={s}"));
    }
    router
        .oneshot(builder.body(Body::empty()).expect("well-formed request"))
        .await
        .expect("in-memory request")
}

/// The `id` of every entry in a list response.
async fn listed_ids(router: Router, session: Option<&str>) -> Vec<String> {
    let resp = get_projects(router, None, session).await;
    assert_eq!(resp.status(), StatusCode::OK, "list must answer 200");
    let body: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json array");
    body.as_array()
        .expect("list body is an array")
        .iter()
        .filter_map(|e| e["id"].as_str())
        .map(str::to_owned)
        .collect()
}

/// AC1 — the ticket's exact repro: a viewer account (created via admin, no
/// project assignments) must get an empty registry, not the full one.
#[tokio::test]
async fn ac1_a_viewer_with_no_memberships_gets_an_empty_registry() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    assert!(
        listed_ids(list_router(app), Some(VIEWER_NOWHERE_SESSION))
            .await
            .is_empty(),
        "a viewer must not enumerate projects it cannot open"
    );
}

/// AC2 — membership is the gate in BOTH directions: carol (member-tier, member
/// of [`PID`]) sees her project; mallory (member-tier, member of
/// [`OTHER_PID`] only, which is not even registered here) sees none of ours.
#[tokio::test]
async fn ac2_a_member_sees_only_the_projects_they_are_assigned_to() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    assert_eq!(
        listed_ids(list_router(app.clone()), Some(MEMBER_SESSION)).await,
        vec![PID.to_owned()],
        "an assigned member keeps seeing their own project"
    );
    assert!(
        listed_ids(list_router(app), Some(OUTSIDE_SESSION))
            .await
            .is_empty(),
        "a member of another project must not enumerate this one"
    );
}

/// AC3 — morgan is manage-tier (Manager) but a member of [`OTHER_PID`] only;
/// the per-project gate 403s her reads of [`PID`], so the list must not name
/// it either (the same scoping the fleet river applies to the lead tier).
#[tokio::test]
async fn ac3_a_manage_tier_non_member_does_not_see_the_project_either() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    let listed = listed_ids(list_router(app), Some(LEAD_ELSEWHERE_SESSION)).await;
    assert!(
        !listed.iter().any(|id| id == PID),
        "the list may never name a project the per-project gate refuses"
    );
}

/// AC4 — Super/Admin bypass the per-project gate, so they see the full
/// registry; the dashboard's project switcher keeps working for them. Exercised
/// for Admin over BOTH credential transports (session cookie and bearer — the
/// transport the REST store runner authenticates with); the Super branch of the
/// same scope decision is covered by `river_scope`'s unit tests in `fleet.rs`.
#[tokio::test]
async fn ac4_an_admin_sees_the_full_registry() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    assert_eq!(
        listed_ids(list_router(app.clone()), Some(INSIDE_SESSION)).await,
        vec![PID.to_owned()],
        "the admin still sees every registered project"
    );
    let resp = get_projects(list_router(app), Some(INSIDE_BEARER), None).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "bearer is a valid credential"
    );
    let body: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json array");
    let ids: Vec<&str> = body
        .as_array()
        .expect("list body is an array")
        .iter()
        .filter_map(|e| e["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec![PID],
        "the scoping applies to bearer-resolved principals too"
    );
}

/// AC5 — broken registrations travel beside the healthy ones and carry a
/// config path and failure reason; they obey the same membership rule.
#[tokio::test]
async fn ac5_broken_entries_follow_the_same_membership_rule() {
    let app = app_with(
        Some(Arc::new(StubAuth)),
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    app.broken.write().await.push(BrokenProject {
        id: OTHER_PID.to_owned(),
        config_path: std::path::PathBuf::from("/w/elsewhere/coxagent.json"),
        error: "invalid config".to_owned(),
    });
    // Admin (alice): sees the healthy project AND the broken marker.
    let seen = listed_ids(list_router(app.clone()), Some(INSIDE_SESSION)).await;
    assert!(
        seen.contains(&PID.to_owned()) && seen.contains(&OTHER_PID.to_owned()),
        "the admin sees healthy and broken entries alike (got {seen:?})"
    );
    // Viewer (vanessa, member of nothing): neither.
    assert!(
        listed_ids(list_router(app), Some(VIEWER_NOWHERE_SESSION))
            .await
            .is_empty(),
        "a broken project's id and config path are not other teams' business"
    );
}

/// AC6 — open mode (no accounts configured) is the local single-operator
/// deployment: the operator sees every project without signing in.
#[tokio::test]
async fn ac6_open_mode_still_lists_every_project() {
    let app = app_with(
        None,
        Arc::new(CountingStore::seeded(ProjectState::default())),
    )
    .await;
    assert_eq!(
        listed_ids(list_router(app), None).await,
        vec![PID.to_owned()],
        "open mode keeps the pre-CXA-B141 behaviour"
    );
}
