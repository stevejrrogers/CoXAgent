// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
//! Product-goal management and the goal-line outcome ledger read surface
//! (CXA-F228): the PO declares goal lines, work binds to them at creation,
//! and the outcomes endpoint aggregates which goals verified deliverables
//! actually advanced — with an explicit unattributed bucket for anything
//! that carries no resolvable association.

use super::*;

/// GET `/api/projects/:pid/goals/outcomes` — per-goal aggregation of verified
/// contributions (zero-contribution goals included) plus the unattributed
/// bucket, each entry with its missing-association reason.
pub(super) async fn outcomes_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match p.store.load().await {
        Ok(state) => Json(state.outcome_report()).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// POST `/api/projects/:pid/goals` — declare a product goal (PO's goal gate).
pub(super) async fn add_goal_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<GoalTitleReq>,
) -> axum::response::Response {
    if super::inbox::gate_principal(
        &app,
        &headers,
        coxagent_application::AuthRole::can_approve_ready,
    )
    .await
    .is_none()
    {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "only the PO's goal gate may declare goals",
        )
            .into_response();
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let title = req.title.trim().to_owned();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "title is required").into_response();
    }
    let mut minted = None;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        match s.add_goal(&title) {
            Ok(gid) => {
                minted = Some(gid);
                Ok(())
            }
            Err(e) => Err(coxagent_application::PortError::Corrupt(e.to_string())),
        }
    })
    .await
    {
        Ok(()) => match minted {
            Some(gid) => {
                Json(serde_json::json!({ "ok": true, "id": gid.to_string(), "title": title }))
                    .into_response()
            }
            None => internal_error("goal add reported success without minting an id"),
        },
        Err(e) => internal_error(&e.to_string()),
    }
}

/// POST `/api/projects/:pid/goals/:gid/rename` — restate a goal's wording.
/// The stable id (and every recorded association) is untouched; this endpoint
/// exists so the rename-severance guarantee (AC3) has a real path to exercise.
pub(super) async fn rename_goal_ep(
    State(app): State<AppState>,
    Path((pid, gid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<GoalTitleReq>,
) -> axum::response::Response {
    if super::inbox::gate_principal(
        &app,
        &headers,
        coxagent_application::AuthRole::can_approve_ready,
    )
    .await
    .is_none()
    {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "only the PO's goal gate may restate goals",
        )
            .into_response();
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(goal_id) = coxagent_domain::GoalId::new(gid.trim()) else {
        return (StatusCode::BAD_REQUEST, "invalid goal id").into_response();
    };
    let title = req.title.trim().to_owned();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "title is required").into_response();
    }
    let mut missing = false;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        match s.rename_goal(&goal_id, &title) {
            Ok(found) => {
                missing = !found;
                Ok(())
            }
            Err(e) => Err(coxagent_application::PortError::Corrupt(e.to_string())),
        }
    })
    .await
    {
        Ok(()) if missing => (StatusCode::NOT_FOUND, "no such goal").into_response(),
        Ok(()) => {
            Json(serde_json::json!({ "ok": true, "id": goal_id.to_string(), "title": title }))
                .into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

/// POST `/api/projects/:pid/ticket/:id/goal` — declare (or re-point) the goal
/// a ticket advances. The creation-time path validates through
/// `AddTicketUseCase`; this is the backfill path that attributes tickets
/// verified before goal-line tracking shipped (CXA-F228 AC5). Attribution of
/// already-delivered work is allowed even to a retired goal — the gate only
/// closes to NEW work.
pub(super) async fn ticket_set_goal_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SetTicketGoalReq>,
) -> axum::response::Response {
    if super::inbox::gate_principal(
        &app,
        &headers,
        coxagent_application::AuthRole::can_approve_ready,
    )
    .await
    .is_none()
    {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "only the PO's goal gate may bind work to goals",
        )
            .into_response();
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(goal_id) = coxagent_domain::GoalId::new(req.goal_id.trim()) else {
        return (StatusCode::BAD_REQUEST, "invalid goal id").into_response();
    };
    let mut problem: Option<(StatusCode, String)> = None;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        if !s.goals.iter().any(|g| g.id == goal_id) {
            problem = Some((StatusCode::BAD_REQUEST, format!("no such goal {goal_id}")));
            return Ok(());
        }
        match s.tickets.iter_mut().find(|t| t.id().as_str() == id) {
            Some(t) => {
                if let Err(e) = t.set_goal_id(coxagent_domain::Role::User, goal_id.clone()) {
                    problem = Some((StatusCode::CONFLICT, e.to_string()));
                }
            }
            None => problem = Some((StatusCode::NOT_FOUND, "no such ticket".to_owned())),
        }
        Ok(())
    })
    .await
    {
        Ok(()) => match problem {
            Some((code, msg)) => (code, msg).into_response(),
            None => {
                Json(serde_json::json!({ "ok": true, "goal": goal_id.to_string() })).into_response()
            }
        },
        Err(e) => internal_error(&e.to_string()),
    }
}
