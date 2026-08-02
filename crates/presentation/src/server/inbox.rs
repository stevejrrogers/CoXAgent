// Part of the server module split by concern — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The hybrid-team inbox: everything waiting on the signed-in PERSON —
//! tickets pending their approval, evidence awaiting their verification,
//! exception tickets routed to them, and questions addressed to them — plus
//! the endpoints their one-click actions call (see docs/HYBRID_TEAM.md).

use super::*;

/// GET `/api/projects/:pid/inbox` — the caller's "waiting for me" queue.
pub(super) async fn inbox_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let me = principal_name(&app, &headers)
        .await
        .unwrap_or_else(|| "operator".to_owned());
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    let human = &cfg.workflow.human;
    let Ok(state) = p.store.load().await else {
        return internal_error("load failed");
    };

    let mut items: Vec<serde_json::Value> = Vec::new();
    for t in &state.tickets {
        let id = t.id().to_string();
        // Ready-gate approvals: designed, waiting in Pending for a person.
        if human.gate_ready
            && t.status() == coxagent_domain::Status::Pending
            && t.design().technical.is_some()
        {
            items.push(serde_json::json!({
                "kind": "approve_ready", "ticket": id, "title": t.title(),
                "priority": format!("{:?}", t.priority()).to_lowercase(),
            }));
            continue;
        }
        // Verify-gate: fixed, evidence attached, waiting for a human verdict.
        if human.gate_verify
            && t.status() == coxagent_domain::Status::Fixed
            && state.ticket_evidence.contains_key(&id)
        {
            items.push(serde_json::json!({
                "kind": "verify", "ticket": id, "title": t.title(),
            }));
            continue;
        }
        // Exception tickets routed to me.
        if t.assignee() == Some(me.as_str()) {
            items.push(serde_json::json!({
                "kind": "assigned", "ticket": id, "title": t.title(),
                "status": format!("{:?}", t.status()).to_lowercase(),
            }));
        }
    }
    // Auto-approved inside the undo window: a person can still pull it back.
    let undo_window = human.adaptive.undo_window_minutes();
    for (id, at) in &state.auto_approved_at {
        let age_min = coxagent_application::use_cases::cycle::seconds_since_public(at)
            .map_or(u64::MAX, |s| s / 60);
        if age_min > undo_window {
            continue;
        }
        if let Some(t) = state.tickets.iter().find(|t| t.id().as_str() == id) {
            items.push(serde_json::json!({
                "kind": "auto_approved", "ticket": id, "title": t.title(),
                "minutes_left": undo_window.saturating_sub(age_min),
            }));
        }
    }
    // Questions addressed to me (`@username`, or my bare username).
    for q in &state.questions {
        if q.answer.is_empty()
            && (q.to.eq_ignore_ascii_case(&me)
                || q.to.eq_ignore_ascii_case(&format!("@{me}")))
        {
            items.push(serde_json::json!({
                "kind": "question", "id": q.id, "ticket": q.ticket,
                "from": q.from, "body": q.body,
                "asked_at": q.asked_at, "escalated": q.escalated,
            }));
        }
    }
    // PRs approved by the SA but held for human eyes.
    if let Some(forge) = &p.forge {
        if let Ok(prs) = forge.list_open_prs().await {
            for pr in prs {
                let approved = state
                    .reviews
                    .iter()
                    .any(|r| r.number == pr.number && r.decision == "approve");
                if approved {
                    items.push(serde_json::json!({
                        "kind": "review_pr", "number": pr.number,
                        "title": pr.title, "url": pr.url,
                    }));
                }
            }
        }
    }
    Json(serde_json::json!({ "user": me, "items": items })).into_response()
}

/// POST `/api/projects/:pid/ticket/:id/ready` — a person approves a designed
/// ticket into `Ready` (the hybrid PO gate).
pub(super) async fn human_ready_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    human_transition(&app, &pid, &id, &headers, coxagent_domain::Status::Ready).await
}

/// POST `/api/projects/:pid/ticket/:id/verify` — a person renders the QA
/// verdict on a fixed ticket (the hybrid verify gate).
pub(super) async fn human_verify_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    human_transition(&app, &pid, &id, &headers, coxagent_domain::Status::Verified).await
}

async fn human_transition(
    app: &AppState,
    pid: &str,
    id: &str,
    headers: &axum::http::HeaderMap,
    to: coxagent_domain::Status,
) -> axum::response::Response {
    let Some(p) = app.project(pid).await else {
        return not_found();
    };
    let me = principal_name(app, headers)
        .await
        .unwrap_or_else(|| "operator".to_owned());
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(t) = state.tickets.iter_mut().find(|t| t.id().as_str() == id) else {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    };
    if let Err(e) = t.transition_to(coxagent_domain::Role::User, to) {
        return (axum::http::StatusCode::CONFLICT, e.to_string()).into_response();
    }
    let label = format!("{to:?}").to_lowercase();
    // Teach the adaptive gate: every human decision is a sample
    // (docs/ADAPTIVE_APPROVAL.md).
    if let Some(t) = state.tickets.iter().find(|t| t.id().as_str() == id) {
        let shape = coxagent_application::use_cases::approval_risk::shape_key(t);
        state.approval_samples.push(
            coxagent_application::use_cases::approval_memory::ApprovalSample {
                shape,
                decision: "approve".to_owned(),
                by: me.clone(),
                reason: String::new(),
                at: coxagent_application::state::now_rfc3339(),
            },
        );
    }
    state.auto_approved_at.remove(id);
    state.log_activity("USER", &format!("{me} moved ticket to {label}"), Some(id.to_owned()));
    state.post_comment(
        "USER",
        &format!("🧑‍⚖️ @{me} approved {id} → {label}."),
        Some(id.to_owned()),
    );
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true, "status": label })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// POST `/api/projects/:pid/ticket/:id/assign` — route a ticket to a person
/// (empty username hands it back to the agent pool).
pub(super) async fn assign_ticket_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    Json(req): Json<AssignReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(t) = state.tickets.iter_mut().find(|t| t.id().as_str() == id) else {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    };
    t.assign_to_human(&req.username);
    let note = if req.username.trim().is_empty() {
        format!("↩️ {id} returned to the agent pool.")
    } else {
        format!("🧑‍💻 {id} assigned to @{}.", req.username.trim())
    };
    state.log_activity("USER", "assignment changed", Some(id.clone()));
    state.post_comment("USER", &note, Some(id));
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct AssignReq {
    #[serde(default)]
    pub(super) username: String,
}

/// POST `/api/projects/:pid/ticket/:id/undo-approval` — pull an
/// auto-approved ticket back to `Pending` inside its undo window. The pull-back
/// is itself the strongest teaching signal: that shape goes back to asking.
pub(super) async fn undo_approval_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let me = principal_name(&app, &headers)
        .await
        .unwrap_or_else(|| "operator".to_owned());
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    if !state.auto_approved_at.contains_key(&id) {
        return (
            axum::http::StatusCode::CONFLICT,
            "not an auto-approved ticket (or the undo window has closed)",
        )
            .into_response();
    }
    let Some(t) = state.tickets.iter_mut().find(|t| t.id().as_str() == id) else {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    };
    let shape = coxagent_application::use_cases::approval_risk::shape_key(t);
    if let Err(e) = t.transition_to(coxagent_domain::Role::User, coxagent_domain::Status::Pending) {
        return (axum::http::StatusCode::CONFLICT, e.to_string()).into_response();
    }
    state.approval_samples.push(
        coxagent_application::use_cases::approval_memory::ApprovalSample {
            shape: shape.clone(),
            decision: "undo".to_owned(),
            by: me.clone(),
            reason: "human pulled back an auto-approval".to_owned(),
            at: coxagent_application::state::now_rfc3339(),
        },
    );
    if !state.ask_again_shapes.contains(&shape) {
        state.ask_again_shapes.push(shape.clone());
    }
    state.auto_approved_at.remove(&id);
    let note = format!(
        "↩️ @{me} undid the auto-approval of {id} — `{shape}` goes back to asking a person."
    );
    state.log_activity("USER", "undid an auto-approval", Some(id.clone()));
    state.post_chat_in(
        "SYSTEM",
        &note,
        coxagent_application::state::AGENTS_CHANNEL,
        Vec::new(),
    );
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}
