//! Lesson efficacy loop endpoints (CXA-F306): the reviewer actions on the
//! Hub lessons surface. Reads ride `GET /api/projects/:pid/metrics/summary`
//! (the `lesson_efficacy` key) — these are the two writes the loop needs:
//! dismissing a suggested incident→lesson match (AC3) and filing the
//! structural prevention ticket for a repeating lesson (AC2's one-click
//! action). Every computation is pure in `lesson_efficacy` — these handlers
//! only authorize, marshal, and persist through the store port.

use super::*;
use axum::response::{IntoResponse, Response};

/// Request body for both lesson actions.
#[derive(serde::Deserialize)]
pub(super) struct LessonActionReq {
    /// The lesson text (the dedupe key across the efficacy ledger).
    #[serde(default)]
    lesson: String,
    /// `IncidentRecord.at` of the match being dismissed.
    #[serde(default)]
    incident_at: String,
    /// What triggered the dismissed incident (kept for the dismissal log).
    #[serde(default)]
    incident_reason: String,
}

/// P5a-style in-handler authorization, mirroring `authorize_brake_call`:
/// with hub auth configured, a lesson write needs a project member with
/// write rights — viewers may read the efficacy view but never steer it.
/// Returns the acting username for the dismissal record.
async fn authorize_lesson_write(
    app: &AppState,
    pid: &str,
    headers: &axum::http::HeaderMap,
) -> Result<String, Response> {
    let Some(auth) = &app.auth else {
        // Open mode (no accounts configured): every request IS the operator.
        return Ok("operator".to_owned());
    };
    let Some(user) = resolve_principal(auth, headers).await else {
        return Err((StatusCode::UNAUTHORIZED, "sign in first").into_response());
    };
    if !user.role.can_write() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "write role required" })),
        )
            .into_response());
    }
    let is_super_or_admin = user.role == coxagent_application::auth::AuthRole::Super
        || user.role == coxagent_application::auth::AuthRole::Admin;
    if !is_super_or_admin && !user.projects.iter().any(|p| p == pid) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "not a member of this project" })),
        )
            .into_response());
    }
    Ok(user.username)
}

/// `POST /api/projects/:pid/lessons/dismiss` — a reviewer rejects a suggested
/// incident→lesson match (AC3): the recurrence it recorded is retracted and
/// the pair is persisted as dismissed, so it can never increment again.
pub(super) async fn lessons_dismiss_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LessonActionReq>,
) -> Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let actor = match authorize_lesson_write(&app, &pid, &headers).await {
        Ok(actor) => actor,
        Err(refused) => return refused,
    };
    if req.lesson.trim().is_empty() || req.incident_at.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "dismiss needs lesson and incident_at" })),
        )
            .into_response();
    }
    let lesson = req.lesson.trim().to_owned();
    let incident_at = req.incident_at.trim().to_owned();
    let incident_reason = req.incident_reason.trim().to_owned();
    let retracted = std::sync::atomic::AtomicBool::new(false);
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        if s.dismiss_match(&lesson, &incident_at, &incident_reason, &actor) {
            retracted.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    })
    .await
    {
        Ok(()) => Json(json!({
            "ok": true,
            "lesson": lesson,
            "incident_at": incident_at,
            "recurrence_retracted": retracted.load(std::sync::atomic::Ordering::Relaxed),
            "dismissed_by": actor,
        }))
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// The escalation still in flight for a lesson, if any: its open prevention
/// ticket. `None` when the lesson has no escalation, or the one it had is
/// closed (the sweep may owe it a higher rung).
async fn open_escalation_of(
    store: &dyn coxagent_application::ports::outbound::StateStorePort,
    lesson: &str,
) -> Option<(String, String)> {
    let state = store.load().await.ok()?;
    let record = state.lesson_records.iter().find(|r| r.text == lesson)?;
    let escalation = record.escalated.as_ref()?;
    let open = state.tickets.iter().any(|t| {
        t.id().as_str() == escalation.ticket
            && !matches!(
                t.status(),
                coxagent_domain::Status::Done
                    | coxagent_domain::Status::Documented
                    | coxagent_domain::Status::Rejected
            )
    });
    open.then(|| (escalation.ticket.clone(), escalation.stage.clone()))
}

/// `POST /api/projects/:pid/lessons/escalate` — the repeating section's
/// one-click action (AC2): file the structural prevention ticket for the
/// lesson and record the escalation on its efficacy ledger. Idempotent: an
/// existing escalation whose ticket is still open is returned as-is, and the
/// shared `AddTicketUseCase` duplicate gate is the second dedupe line.
pub(super) async fn lessons_escalate_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LessonActionReq>,
) -> Response {
    use coxagent_domain::ticket::{Complexity, Priority, TicketType};
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match authorize_lesson_write(&app, &pid, &headers).await {
        Ok(_) => {}
        Err(refused) => return refused,
    }
    let lesson = req.lesson.trim().to_owned();
    if lesson.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "escalate needs a lesson" })),
        )
            .into_response();
    }
    if let Some((ticket, stage)) = open_escalation_of(p.store.as_ref(), &lesson).await {
        return Json(json!({ "ok": true, "ticket": ticket, "stage": stage, "already": true }))
            .into_response();
    }
    let title = format!(
        "Prevention: {}",
        lesson.chars().take(80).collect::<String>()
    );
    let adder =
        coxagent_application::use_cases::AddTicketUseCase::new(std::sync::Arc::clone(&p.store));
    match adder
        .execute(coxagent_application::use_cases::AddTicketInput {
            ticket_type: TicketType::Chore,
            title: title.clone(),
            description: format!(
                "CXA-F306 lesson efficacy: this lesson keeps recurring — its failure class \
                 matched 2+ incidents (or was re-learned twice) after it was recorded. \
                 Stop the class, not the symptom.\n\nLesson: {lesson}"
            ),
            priority: Priority::Medium,
            complexity: Complexity::Medium,
            has_ui: false,
            acceptance_criteria: vec![
                "The lesson's failure class cannot recur without being caught by a \
                 check, not a lesson"
                    .to_owned(),
            ],
            goal: None,
        })
        .await
    {
        Ok(ticket) => {
            let _ = coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
                s.mark_escalated(&lesson, ticket.as_str(), "chore");
                Ok(())
            })
            .await;
            Json(json!({ "ok": true, "ticket": ticket.as_str(), "stage": "chore" })).into_response()
        }
        Err(e)
            if e.to_string()
                .contains(coxagent_application::use_cases::add_ticket::DUPLICATE_REFUSED) =>
        {
            // A structurally identical prevention ticket is already open —
            // point the reviewer at it instead of erroring.
            if let Ok(state) = p.store.load().await {
                if let Some(t) = state
                    .tickets
                    .iter()
                    .find(|t| t.title() == title && t.ticket_type() == TicketType::Chore)
                {
                    let _ = coxagent_application::ports::outbound::mutate_state(
                        p.store.as_ref(),
                        |s| {
                            s.mark_escalated(&lesson, t.id().as_str(), "chore");
                            Ok(())
                        },
                    )
                    .await;
                    return Json(json!({
                        "ok": true,
                        "ticket": t.id().as_str(),
                        "stage": "chore",
                        "already": true,
                    }))
                    .into_response();
                }
            }
            internal_error(&e.to_string())
        }
        Err(e) => internal_error(&e.to_string()),
    }
}
