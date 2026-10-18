// Part of the server module split by concern — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The adaptive-approval policy surface (CXA-F303): the operator reads what
//! the gate learned, forces a shape back to always-ask, and releases that
//! override. Every computation is pure in `use_cases/approval_policy` —
//! these handlers only authorize, marshal, and persist through the store
//! port, exactly like the brake cockpit beside which this pattern shipped.

use super::*;
use axum::response::{IntoResponse, Response};

/// The project's human-gate settings as the pass sees them: `gate_ready`
/// plus the adaptive section. Read from the project's `coxagent.json` the
/// same way the inbox reads it (unparseable/missing = defaults, never a
/// guessed policy).
fn human_gate(p: &ProjectHandle) -> (bool, coxagent_application::config::AdaptiveConfig) {
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    let human = &cfg.workflow.human;
    (human.gate_ready, human.adaptive.clone())
}

/// `GET /api/projects/:pid/approval-policy` — what the adaptive gate learned:
/// every shape's rule, the samples and deciders behind it, what the loop is
/// deciding alone today, and the undo window. Any signed-in caller may read
/// it (transparency is the point); no auth configured = operator.
pub(super) async fn approval_policy_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    if gate_principal(&app, &headers, |_| true).await.is_none() {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    }
    let (gate_ready, adaptive) = human_gate(&p);
    match p.store.load().await {
        Ok(state) => Json(
            coxagent_application::use_cases::approval_policy::policy_overview(
                &state,
                &adaptive,
                gate_ready,
                &coxagent_application::state::now_rfc3339(),
            ),
        )
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// One operator-supplied shape on a policy flip.
#[derive(serde::Deserialize)]
pub(super) struct ShapeReq {
    #[serde(default)]
    pub(super) shape: String,
}

/// Validate the body's shape against the vocabulary `shape_key` can produce,
/// so an override that nothing will ever match (`tests/small`) can never be
/// written. `Err` carries the message for the 400 body, built from the same
/// constants — no drift.
fn validated_shape(req: &ShapeReq) -> Result<String, String> {
    use coxagent_application::use_cases::approval_risk::{
        is_valid_shape, SHAPE_COMPLEXITIES, SHAPE_KINDS,
    };
    let shape = req.shape.trim().to_owned();
    if is_valid_shape(&shape) {
        return Ok(shape);
    }
    Err(format!(
        "unknown ticket shape {:?} — use <kind>/<complexity> with kind one of {} and \
         complexity one of {} (e.g. \"test/small\")",
        req.shape,
        SHAPE_KINDS.join(", "),
        SHAPE_COMPLEXITIES.join(", ")
    ))
}

/// Which way the operator is flipping a shape's override.
#[derive(Debug, Clone, Copy)]
enum Flip {
    /// Add the shape to `ask_again_shapes`: the gate stops auto-approving it.
    Force,
    /// Remove it: the gate re-learns from the recorded decisions.
    Release,
}

/// Shared body of the two flips: force a shape to always-ask, or release the
/// override so the gate re-learns from existing history (release deliberately
/// does NOT zero the samples — undo samples keep speaking for themselves).
/// Idempotent: `changed` reports whether state moved. The override is
/// `ask_again_shapes` — the same field the undo path writes, honored by the
/// next cycle pass with no restart.
async fn flip_override(
    app: &AppState,
    pid: &str,
    headers: &axum::http::HeaderMap,
    req: ShapeReq,
    action: Flip,
) -> Response {
    let force = matches!(action, Flip::Force);
    let Some(p) = app.project(pid).await else {
        return not_found();
    };
    // A flip retires (or restores) a learned gate rule — the same weight as
    // undoing one auto-approval, so the same qualification.
    let Some(me) = gate_principal(
        app,
        headers,
        coxagent_application::AuthRole::can_approve_ready,
    )
    .await
    else {
        return (
            StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response();
    };
    let shape = match validated_shape(&req) {
        Ok(shape) => shape,
        Err(message) => return bad_request_error(&message),
    };
    let verb = if force { "forced" } else { "released" };
    let changed = std::sync::atomic::AtomicBool::new(false);
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        if force {
            if !s.ask_again_shapes.iter().any(|x| x == &shape) {
                s.ask_again_shapes.push(shape.clone());
                changed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        } else {
            let before = s.ask_again_shapes.len();
            s.ask_again_shapes.retain(|x| x != &shape);
            changed.store(
                s.ask_again_shapes.len() != before,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        if changed.load(std::sync::atomic::Ordering::Relaxed) {
            let note = if force {
                format!("🔇 @{me} forced `{shape}` tickets back to always-ask — the gate stops auto-approving that shape from the next pass.")
            } else {
                format!("🔓 @{me} released the always-ask override on `{shape}` — the gate re-learns from the recorded decisions.")
            };
            s.log_activity("USER", &format!("{verb} the always-ask override on {shape}"), None);
            s.post_chat_in(
                "SYSTEM",
                &note,
                coxagent_application::state::APPROVALS_CHANNEL,
                Vec::new(),
            );
        }
        Ok(())
    })
    .await
    {
        Ok(()) => Json(json!({
            "ok": true,
            "shape": shape,
            "overridden": force,
            "changed": changed.load(std::sync::atomic::Ordering::Relaxed),
        }))
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `POST /api/projects/:pid/approval-policy/ask-again` — force a shape back
/// to always-ask. Body `{"shape":"test/small"}`; idempotent.
pub(super) async fn approval_policy_ask_again_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ShapeReq>,
) -> Response {
    flip_override(&app, &pid, &headers, req, Flip::Force).await
}

/// `POST /api/projects/:pid/approval-policy/release` — flip a shape BACK to
/// its learned rule by removing the always-ask override. Body
/// `{"shape":"test/small"}`; idempotent; recorded history re-speaks.
pub(super) async fn approval_policy_release_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ShapeReq>,
) -> Response {
    flip_override(&app, &pid, &headers, req, Flip::Release).await
}
