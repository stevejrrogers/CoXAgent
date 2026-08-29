//! The brake cockpit (CXA-F238): the read model behind the dashboard's brake
//! cards plus bounded operator holds/overrides over the self-tuning brakes.
//! Every computation is pure in `metrics_brakes` — these handlers only
//! authorize, marshal, and persist through the store port.

use super::*;
use axum::response::{IntoResponse, Response};
use std::sync::atomic::{AtomicBool, Ordering};

/// Request to set a hold on one brake.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrakeHoldReq {
    /// `null` freezes the brake at its current value; `true`/`false` pins it.
    #[serde(default)]
    pinned_value: Option<bool>,
    /// Why — recorded verbatim in the audit trail.
    #[serde(default)]
    reason: String,
    /// RFC3339 bound past which the hold stops applying (mandatory, future).
    #[serde(default)]
    expires_at: String,
}

/// P5a-style in-handler authorization (defense in depth mirroring
/// `authorize_store_call`): when hub auth is configured, every brake call
/// needs a principal who is a member of `:pid`; WRITES additionally need
/// ordinary write rights — viewers may inspect the cockpit but never steer
/// it. Returns the acting username for the audit trail.
async fn authorize_brake_call(
    app: &AppState,
    pid: &str,
    headers: &axum::http::HeaderMap,
    write: bool,
) -> Result<String, Response> {
    let Some(auth) = &app.auth else {
        // Open mode (no accounts configured): every request IS the operator.
        return Ok("operator".to_owned());
    };
    let Some(user) = resolve_principal(auth, headers).await else {
        return Err((StatusCode::UNAUTHORIZED, "sign in first").into_response());
    };
    if write && !user.role.can_write() {
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

/// `GET /api/projects/:pid/brakes` — each brake's effective state, the eval
/// signals + hysteresis thresholds that produced it, what the loop would do
/// this pass, and the active operator holds. Viewer-readable.
pub(super) async fn brakes_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    if let Err(refused) = authorize_brake_call(&app, &pid, &headers, false).await {
        return refused;
    }
    match p.store.load().await {
        Ok(state) => {
            let cockpit = coxagent_application::metrics::brake_cockpit(
                &state,
                &now_rfc3339(),
            );
            Json(cockpit).into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `POST /api/projects/:pid/brakes/:brake/hold` — set a bounded freeze
/// (`pinnedValue: null`) or override (`true`/`false`) on one brake. The hold
/// takes effect immediately, rides above the autonomous decision until its
/// bound, and lands in the audit trail with actor, window and reason.
pub(super) async fn brake_hold_ep(
    State(app): State<AppState>,
    Path((pid, brake)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<BrakeHoldReq>,
) -> Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let actor = match authorize_brake_call(&app, &pid, &headers, true).await {
        Ok(actor) => actor,
        Err(refused) => return refused,
    };
    if !coxagent_application::metrics::is_brake_field(&brake) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unknown brake — use bugs_first or skip_ba" })),
        )
            .into_response();
    }
    let reason = req.reason.trim().to_owned();
    if reason.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "a hold needs a reason — it is the audit trail" })),
        )
            .into_response();
    }
    let bound = time::OffsetDateTime::parse(
        &req.expires_at,
        &time::format_description::well_known::Rfc3339,
    );
    let Ok(bound) = bound else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "expiresAt must be an RFC3339 timestamp" })),
        )
            .into_response();
    };
    let now = time::OffsetDateTime::now_utc();
    if bound <= now {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "expiresAt must be in the future — a hold is always bounded" })),
        )
            .into_response();
    }
    let hold = coxagent_application::state::BrakeHold {
        pinned_value: req.pinned_value,
        reason,
        actor,
        at: now_rfc3339(),
        expires_at: req.expires_at.clone(),
    };
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        if coxagent_application::metrics::set_brake_hold(s, &brake, hold.clone()) {
            Ok(())
        } else {
            Err(coxagent_application::PortError::Corrupt(format!(
                "unknown brake: {brake}"
            )))
        }
    })
    .await
    {
        Ok(()) => Json(json!({
            "ok": true,
            "brake": brake,
            "pinnedValue": req.pinned_value,
            "expiresAt": req.expires_at,
        }))
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `DELETE /api/projects/:pid/brakes/:brake/hold` — release an active hold
/// now; the brake recomposes straight back to the autonomous decision (no
/// waiting for the next daily pass), audited with source `clear`.
pub(super) async fn brake_hold_clear_ep(
    State(app): State<AppState>,
    Path((pid, brake)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let actor = match authorize_brake_call(&app, &pid, &headers, true).await {
        Ok(actor) => actor,
        Err(refused) => return refused,
    };
    if !coxagent_application::metrics::is_brake_field(&brake) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unknown brake — use bugs_first or skip_ba" })),
        )
            .into_response();
    }
    let cleared = AtomicBool::new(false);
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        if coxagent_application::metrics::clear_brake_hold(s, &brake, &actor) {
            cleared.store(true, Ordering::Relaxed);
        }
        Ok(())
    })
    .await
    {
        Ok(()) if cleared.load(Ordering::Relaxed) => {
            Json(json!({ "ok": true, "brake": brake, "cleared": true })).into_response()
        }
        Ok(()) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("no active hold on {brake}") })),
        )
            .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}
