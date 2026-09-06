// Part of the server module split by concern — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The human claim lifecycle over HTTP (CXA-F283): `takeover` pulls a
//! claimable ticket into the caller's own `InProgress` claim (or takes over a
//! stalled claim), `handback` completes the work a person did by hand and
//! returns the ticket to the loop. Both are person-gated through the shared
//! [`super::inbox::gate_principal`] like every sibling endpoint, and both
//! refuse to race a runner that is actively executing the ticket.

use super::*;

/// Who is calling, and whether their role is manage tier. The second gate call
/// re-runs the same principal resolution against `AuthRole::can_manage` —
/// on a hub with no accounts configured both read as the operator, who may
/// manage everything.
async fn claim_gate(
    app: &AppState,
    headers: &axum::http::HeaderMap,
) -> Option<(String, bool)> {
    let me = super::inbox::gate_principal(app, headers, |_| true).await?;
    let can_manage = super::inbox::gate_principal(
        app,
        headers,
        coxagent_application::AuthRole::can_manage,
    )
    .await
    .is_some();
    Some((me, can_manage))
}

/// Map a claim use-case refusal onto the status codes its sibling
/// person-gated endpoints answer with (hold/reject/assign parity).
fn claim_error(e: coxagent_application::use_cases::ClaimError) -> axum::response::Response {
    use coxagent_application::use_cases::ClaimError as CE;
    match e {
        CE::NotFound => (StatusCode::NOT_FOUND, "no such ticket").into_response(),
        CE::NotPermitted => (
            StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response(),
        CE::Conflict(why) => (StatusCode::CONFLICT, why).into_response(),
        CE::Store(e) => internal_error(&e.to_string()),
    }
}

/// POST `/api/projects/:pid/ticket/:id/takeover` — pull a claimable ticket (a
/// `Ready` feature/chore or an `Open` bug) into the caller's own `InProgress`
/// claim as `account@host`, or take over a stalled claim: a manager, or the
/// account whose own run holds it, swaps the holder in one atomic step and the
/// response names the displaced `previous_holder`. Refused with 409 while this
/// hub's runner is actively working the ticket — pause it first; a paused,
/// stopped or crashed run can be taken over.
///
/// Deploy of the human's work follows the existing deploy triggers (the next
/// shipped ticket or a failed-deploy retry) — deliberately out of scope here.
pub(super) async fn takeover_ticket_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(tid) = coxagent_domain::TicketId::new(id) else {
        return (StatusCode::BAD_REQUEST, "bad id").into_response();
    };
    let Some((me, can_manage)) = claim_gate(&app, &headers).await else {
        return (
            StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response();
    };
    // Live fact from this hub's runner: only a RUNNING runner blocks a
    // takeover, and only when it holds this very ticket. The snapshot is read
    // once — mode and identity travel together into the atomic decision.
    let snap = p.runner.snapshot();
    let active_runner = (snap.mode == "running").then(|| p.runner.worker_id());
    // The human claim stamps `account@host` like every claim; the host is the
    // hub this person worked through.
    let host = snap.host.clone().unwrap_or_else(|| "local".to_owned());
    let worker = format!("{me}@{host}");
    let uc = coxagent_application::use_cases::ClaimsUseCase::new(Arc::clone(&p.store));
    match uc
        .takeover_claim(
            &tid,
            &worker,
            &coxagent_application::state::now_rfc3339(),
            can_manage,
            &me,
            active_runner.as_deref(),
        )
        .await
    {
        Ok(out) => {
            let ticket = match serde_json::to_value(&out.ticket) {
                Ok(v) => v,
                Err(e) => return internal_error(&e.to_string()),
            };
            Json(json!({
                "ok": true,
                "ticket": ticket,
                "claimed_by": out.claimed_by,
                "claimed_at": out.claimed_at,
                "previous_holder": out.previous_holder,
            }))
            .into_response()
        }
        Err(e) => claim_error(e),
    }
}

/// The handback body: a non-empty implementation note (the handback's audit
/// trail) plus an optional branch/commit ref naming where the work lives.
#[derive(serde::Deserialize, Default)]
pub(super) struct HandbackReq {
    #[serde(default)]
    pub(super) reason: String,
    #[serde(default, rename = "ref")]
    pub(super) branch_ref: String,
}

/// POST `/api/projects/:pid/ticket/:id/handback` — complete the work a person
/// did by hand: the feature/chore lands `Done` (the DOCS queue picks it up),
/// a bug `Fixed` (TEST verification), the claim clears, and the mandatory
/// implementation note (+ optional branch/commit `ref`) is recorded on the
/// ticket thread in the same write. Same gate, same runner guard.
///
/// Deploy of the human's work follows the existing deploy triggers —
/// deliberately out of scope here.
pub(super) async fn handback_ticket_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: Option<Json<HandbackReq>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(tid) = coxagent_domain::TicketId::new(id) else {
        return (StatusCode::BAD_REQUEST, "bad id").into_response();
    };
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let note = req.reason.trim();
    if note.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "an implementation note is required — it is the handback's audit trail",
        )
            .into_response();
    }
    let Some((me, can_manage)) = claim_gate(&app, &headers).await else {
        return (
            StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response();
    };
    let snap = p.runner.snapshot();
    let active_runner = (snap.mode == "running").then(|| p.runner.worker_id());
    let uc = coxagent_application::use_cases::ClaimsUseCase::new(Arc::clone(&p.store));
    match uc
        .handback(
            &tid,
            note,
            req.branch_ref.trim(),
            can_manage,
            &me,
            active_runner.as_deref(),
        )
        .await
    {
        Ok(out) => Json(json!({ "ok": true, "status": out.status })).into_response(),
        Err(e) => claim_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::store_rpc_test_support::{
        app_with, body_text, CountingStore, StubAuth, INSIDE_SESSION, MEMBER_SESSION,
        VIEWER_NOWHERE_SESSION, PID,
    };
    use axum::body::Body;
    use coxagent_application::ports::outbound::StateStorePort;
    use coxagent_application::state::ProjectState;
    use coxagent_domain::ticket::Ticket;
    use coxagent_domain::{Complexity, Priority, Role, Status, TechnicalDesign, TicketId, TicketType};
    use std::sync::Arc;
    use tower::ServiceExt;

    const NOW: &str = "2026-09-06T12:00:00Z";

    fn ready_feature(id: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t");
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("d");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t
    }

    /// A claimed `InProgress` feature — a stalled run waiting to be taken over.
    fn claimed_feature(id: &str, holder: &str) -> Ticket {
        let mut t = ready_feature(id);
        t.claim(Role::DevFeature, holder, NOW).expect("claim");
        t
    }

    fn claimed_bug(id: &str, holder: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Bug,
            "b",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("bug");
        t.claim(Role::DevBug, holder, NOW).expect("claim");
        t
    }

    async fn app_for(state: ProjectState) -> (AppState, Arc<CountingStore>) {
        let store = Arc::new(CountingStore::seeded(state));
        let app = app_with(
            Some(Arc::new(StubAuth)),
            Arc::clone(&store) as Arc<dyn StateStorePort>,
        )
        .await;
        (app, store)
    }

    /// The deployed shape: both routes behind `auth_mw`, exactly as
    /// `serve_full` layers them.
    fn claims_router(state: AppState) -> Router {
        Router::new()
            .route(
                "/api/projects/:pid/ticket/:id/takeover",
                post(takeover_ticket_ep),
            )
            .route(
                "/api/projects/:pid/ticket/:id/handback",
                post(handback_ticket_ep),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                auth_mw,
            ))
            .with_state(state)
    }

    async fn post_at(
        router: Router,
        path: &str,
        body: Option<&str>,
        session: Option<&str>,
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = session {
            builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={token}"));
        }
        router
            .oneshot(
                builder
                    .body(Body::from(body.unwrap_or_default().to_owned()))
                    .expect("well-formed request"),
            )
            .await
            .expect("in-process request")
    }

    #[tokio::test]
    async fn no_session_is_refused_401_like_hold_and_reject() {
        let (app, _) = app_for(ProjectState {
            tickets: vec![ready_feature("CXC-F1")],
            ..ProjectState::default()
        })
        .await;
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F1/takeover"),
            None,
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_viewer_is_refused_403() {
        // Vanessa (Viewer, member of nothing) is refused at the project gate.
        let (app, _) = app_for(ProjectState {
            tickets: vec![ready_feature("CXC-F1")],
            ..ProjectState::default()
        })
        .await;
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F1/takeover"),
            None,
            Some(VIEWER_NOWHERE_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_member_without_the_holder_is_refused_403_by_the_gate() {
        // Carol (member tier) may not pull fresh work — unclaimed tickets are
        // managers-only — and may not touch alice's stalled claim.
        let (app, _) = app_for(ProjectState {
            tickets: vec![ready_feature("CXC-F1"), claimed_feature("CXC-F2", "alice@mac")],
            ..ProjectState::default()
        })
        .await;
        for id in ["CXC-F1", "CXC-F2"] {
            let resp = post_at(
                claims_router(app.clone()),
                &format!("/api/projects/{PID}/ticket/{id}/takeover"),
                None,
                Some(MEMBER_SESSION),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{id}");
        }
    }

    #[tokio::test]
    async fn takeover_claims_a_ready_feature_for_the_caller() {
        let (app, store) = app_for(ProjectState {
            tickets: vec![ready_feature("CXC-F1")],
            ..ProjectState::default()
        })
        .await;
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F1/takeover"),
            None,
            Some(INSIDE_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = serde_json::from_str::<serde_json::Value>(&body_text(resp).await)
            .expect("json body");
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["claimed_by"], json!("alice@local"));
        assert_eq!(body["previous_holder"], json!(null));
        assert_eq!(body["ticket"]["status"], json!("in_progress"));
        let t = store
            .snapshot()
            .ticket(&TicketId::new("CXC-F1").expect("id"))
            .expect("t")
            .clone();
        assert_eq!(t.status(), Status::InProgress);
        assert_eq!(t.claimed_by(), Some("alice@local"));
    }

    #[tokio::test]
    async fn takeover_takes_over_a_stalled_claim_and_names_the_previous_holder() {
        let (app, store) = app_for(ProjectState {
            tickets: vec![claimed_feature("CXC-F1", "dev@mac")],
            ..ProjectState::default()
        })
        .await;
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F1/takeover"),
            None,
            Some(INSIDE_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = serde_json::from_str::<serde_json::Value>(&body_text(resp).await)
            .expect("json body");
        assert_eq!(body["previous_holder"], json!("dev@mac"));
        assert_eq!(store.snapshot().tickets[0].claimed_by(), Some("alice@local"));
    }

    #[tokio::test]
    async fn takeover_is_refused_while_the_runner_is_working_the_ticket() {
        let (app, _) = app_for(ProjectState {
            tickets: vec![claimed_feature("CXC-F1", "op@mac")],
            ..ProjectState::default()
        })
        .await;
        // This hub's runner is RUNNING and holds the ticket (its identity).
        let p = app.project(PID).await.expect("project");
        p.runner.set_operator("op", "mac");
        p.runner.resume();
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F1/takeover"),
            None,
            Some(INSIDE_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(body_text(resp).await.contains("pause it first"));
    }

    #[tokio::test]
    async fn takeover_refuses_a_non_claimable_status() {
        // A fresh feature sits on Pending (design gate) — nothing to take.
        let pending = Ticket::new(
            TicketId::new("CXC-F2").expect("id"),
            TicketType::Feature,
            "gated",
            "",
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("pending");
        let (app, _) = app_for(ProjectState {
            tickets: vec![pending],
            ..ProjectState::default()
        })
        .await;
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F2/takeover"),
            None,
            Some(INSIDE_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn takeover_of_an_unknown_ticket_is_404() {
        let (app, _) = app_for(ProjectState::default()).await;
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F9/takeover"),
            None,
            Some(INSIDE_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn handback_without_a_note_is_400() {
        let (app, _) = app_for(ProjectState {
            tickets: vec![claimed_feature("CXC-F1", "carol@mac")],
            ..ProjectState::default()
        })
        .await;
        for body in [None, Some("{}"), Some(r#"{"reason":"  "}"#)] {
            let resp = post_at(
                claims_router(app.clone()),
                &format!("/api/projects/{PID}/ticket/CXC-F1/handback"),
                body,
                Some(INSIDE_SESSION),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {body:?}");
        }
    }

    #[tokio::test]
    async fn handback_completes_a_feature_as_done_and_records_the_note() {
        let (app, store) = app_for(ProjectState {
            tickets: vec![claimed_feature("CXC-F1", "carol@mac")],
            ..ProjectState::default()
        })
        .await;
        // Carol's own account holds the claim — she may hand her own work back.
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F1/handback"),
            Some(r#"{"reason":"built it by hand","ref":"feat/human"}"#),
            Some(MEMBER_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = serde_json::from_str::<serde_json::Value>(&body_text(resp).await)
            .expect("json body");
        assert_eq!(body["status"], json!("done"));

        let snap = store.snapshot();
        let t = snap
            .ticket(&TicketId::new("CXC-F1").expect("id"))
            .expect("t")
            .clone();
        assert_eq!(t.status(), Status::Done);
        assert_eq!(t.claimed_by(), None, "the handback clears the claim");
        let note = snap
            .comments
            .iter()
            .find(|c| c.ticket.as_deref() == Some("CXC-F1"))
            .expect("handback comment on the ticket thread");
        assert!(note.body.contains("handed back to the loop"));
        assert!(note.body.contains("built it by hand"));
        assert!(note.body.contains("feat/human"));
        assert!(snap
            .activity
            .iter()
            .any(|a| a.agent == "carol" && a.ticket.as_deref() == Some("CXC-F1")));
    }

    #[tokio::test]
    async fn handback_completes_a_bug_as_fixed() {
        let (app, _) = app_for(ProjectState {
            tickets: vec![claimed_bug("CXC-B1", "carol@mac")],
            ..ProjectState::default()
        })
        .await;
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-B1/handback"),
            Some(r#"{"reason":"fixed at source"}"#),
            Some(MEMBER_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_text(resp).await.contains("\"fixed\""));
    }

    #[tokio::test]
    async fn handback_refused_off_in_progress() {
        let (app, _) = app_for(ProjectState {
            tickets: vec![ready_feature("CXC-F1")],
            ..ProjectState::default()
        })
        .await;
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F1/handback"),
            Some(r#"{"reason":"nothing was in flight"}"#),
            Some(INSIDE_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn handback_of_a_foreign_claim_is_403() {
        // Carol may not complete alice's stalled run.
        let (app, _) = app_for(ProjectState {
            tickets: vec![claimed_feature("CXC-F1", "alice@mac")],
            ..ProjectState::default()
        })
        .await;
        let resp = post_at(
            claims_router(app),
            &format!("/api/projects/{PID}/ticket/CXC-F1/handback"),
            Some(r#"{"reason":"not mine"}"#),
            Some(MEMBER_SESSION),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
