// Part of the server module split by concern — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The hybrid-team inbox: everything waiting on the signed-in PERSON —
//! tickets pending their approval, evidence awaiting their verification,
//! exception tickets routed to them, and questions addressed to them — plus
//! the endpoints their one-click actions call (see docs/HYBRID_TEAM.md).

use super::*;

/// Whether the signed-in caller may take a gate decision, and who they are.
///
/// `None` means refuse. On a hub with no accounts configured every request IS
/// the operator — the gate cannot mean anything there, so it stands aside.
pub(super) async fn gate_principal(
    app: &AppState,
    headers: &axum::http::HeaderMap,
    allowed: fn(coxagent_application::AuthRole) -> bool,
) -> Option<String> {
    let Some(auth) = app.auth.clone() else {
        return Some("operator".to_owned());
    };
    let user = resolve_principal(&auth, headers).await?;
    allowed(user.role).then_some(user.username)
}

/// GET `/api/projects/:pid/inbox` — the caller's "waiting for me" queue.
#[allow(clippy::too_many_lines)] // one linear pass building each inbox item kind
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
    // The caller's role decides which items they may ACT on — but every item is
    // still SHOWN to everyone, so the whole team sees the queue and only the
    // right role gets an enabled button. Open mode (no auth) is the operator,
    // who may do everything.
    let my_role = match app.auth.clone() {
        Some(auth) => resolve_principal(&auth, &headers)
            .await
            .map_or(coxagent_application::auth::AuthRole::Viewer, |u| u.role),
        None => coxagent_application::auth::AuthRole::Super,
    };
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
        // Cost gate: a ticket whose estimated run cost exceeds the approval
        // threshold is HELD until a person okays the spend. Surface every held
        // ticket here — otherwise they pile up invisibly and the whole queue
        // stalls before any of it reaches DEV (the "spins but never ships" bug).
        if let Some(est) = state.cost_holds.get(&id) {
            // Only while the ticket is still awaiting work — a hold left on a
            // ticket that was since rejected/finished is stale and must not keep
            // showing up as something to approve.
            let live = matches!(
                t.status(),
                coxagent_domain::Status::Pending | coxagent_domain::Status::Ready
            );
            if live && !state.cost_approved.contains(&id) {
                items.push(serde_json::json!({
                    "kind": "cost_approve", "ticket": id, "title": t.title(),
                    "priority": format!("{:?}", t.priority()).to_lowercase(),
                    "estimate_usd": est,
                    "role": "PO", "can_act": my_role.can_approve_ready(),
                }));
                continue;
            }
        }
        // Ready-gate approvals: designed, waiting in Pending for a person.
        if human.gate_ready
            && t.status() == coxagent_domain::Status::Pending
            && t.design().technical.is_some()
        {
            items.push(serde_json::json!({
                "kind": "approve_ready", "ticket": id, "title": t.title(),
                "priority": format!("{:?}", t.priority()).to_lowercase(),
                "role": "BA/PO", "can_act": my_role.can_approve_ready(),
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
                "role": "QA", "can_act": my_role.can_verify(),
            }));
            continue;
        }
        // On hold: parked on an outside blocker — the resume decision is a
        // person's, so it lives in the inbox instead of only a board filter.
        if t.status() == coxagent_domain::Status::OnHold {
            items.push(serde_json::json!({
                "kind": "on_hold", "ticket": id, "title": t.title(),
                "reason": state.hold_reasons.get(&id).cloned().unwrap_or_default(),
                "role": "PO", "can_act": my_role.can_approve_ready(),
            }));
            continue;
        }
        // Exception tickets routed to me.
        if t.assignee() == Some(me.as_str()) {
            items.push(serde_json::json!({
                "kind": "assigned", "ticket": id, "title": t.title(),
                "status": format!("{:?}", t.status()).to_lowercase(),
                "role": "you", "can_act": true,
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
                "role": "BA/PO", "can_act": my_role.can_approve_ready(),
            }));
        }
    }
    // Questions addressed to me (`@username`, or my bare username). A
    // question held for my focus-window digest (CXA-F176) still shows here —
    // the queue stays honest — but carries `deferred` so the UI renders it
    // as queued-for-digest rather than a fresh interrupt.
    for q in &state.questions {
        if q.answer.is_empty()
            && (q.to.eq_ignore_ascii_case(&me) || q.to.eq_ignore_ascii_case(&format!("@{me}")))
        {
            items.push(serde_json::json!({
                "kind": "question", "id": q.id, "ticket": q.ticket,
                "from": q.from, "body": q.body,
                "asked_at": q.asked_at, "escalated": q.escalated,
                "deferred": q.deferred,
                "role": "you", "can_act": true,
            }));
        }
    }
    // PRs the machine approved but refuses to land alone (`needs_human_eyes`):
    // shown with the gate's REASON and one-click land/dismiss. Sourced from
    // state so it works even on a hub with no forge credentials.
    for (n, reason) in &state.human_holds {
        let pr = state.open_prs.iter().find(|p| p.number == *n);
        items.push(serde_json::json!({
            "kind": "human_eyes", "number": n,
            "title": pr.map(|p| p.title.clone()).unwrap_or_default(),
            "url": pr.map(|p| p.url.clone()).unwrap_or_default(),
            "reason": reason,
            "role": "SA/dev", "can_act": my_role.can_review(),
        }));
    }
    // Merged-then-reverted work (CXA-F047): the scan suspected a shipped
    // ticket's work was undone. Only a person can confirm it — the verdict is
    // what planning is allowed to learn from, so unconfirmed suspicions wait
    // here instead of silently weighting the next sprint.
    for ev in state
        .reverted_work
        .iter()
        .filter(|e| e.decision == coxagent_application::state::RevertDecision::Pending)
    {
        items.push(serde_json::json!({
            "kind": "reverted_work", "sha": ev.sha,
            "ticket": ev.ticket, "subject": ev.subject,
            "role": ev.role, "at": ev.reverted_at,
            "can_act": my_role.can_review(),
        }));
    }
    // PRs approved by the SA but held for human eyes.
    if let Some(forge) = &p.forge {
        if let Ok(prs) = forge.list_open_prs().await {
            for pr in prs {
                // Already surfaced above with its hold reason.
                if state.human_holds.contains_key(&pr.number) {
                    continue;
                }
                let approved = state
                    .reviews
                    .iter()
                    .any(|r| r.number == pr.number && r.decision == "approve");
                if approved {
                    items.push(serde_json::json!({
                        "kind": "review_pr", "number": pr.number,
                        "title": pr.title, "url": pr.url,
                        "role": "SA/dev", "can_act": my_role.can_review(),
                    }));
                    continue;
                }
                // PRs the team has given up on. The fix ladder ends at "tell a
                // human", which fired ONE notification and then skipped the PR
                // every cycle forever — three of them sat open for a week that
                // way, holding the queue against the WIP limit and pausing new
                // dev work, while nothing on any screen said so. A dead end has
                // to be visible, and it stays visible until the PR is gone.
                let attempts = state.pr_fix_attempts.get(&pr.number).copied().unwrap_or(0);
                let rescued = state.pr_rescues.get(&pr.number).copied().unwrap_or(0);
                if attempts >= 3 && rescued >= 1 {
                    items.push(serde_json::json!({
                        "kind": "pr_stuck", "number": pr.number,
                        "title": pr.title, "url": pr.url,
                        "attempts": attempts,
                        "mergeable": pr.mergeable,
                        "role": "SA/dev", "can_act": my_role.can_review(),
                    }));
                } else if pr.mergeable {
                    // Auto-merge held the PR for another gate (merged-result
                    // verification, CI, a competing PR) but didn't abandon it:
                    // the SA either asked for changes at an unmoved head or the
                    // sweep skipped it, so it sat parked and invisible while the
                    // user waited for a notification. If it is genuinely
                    // landable now, surface it — the person is always told a
                    // mergeable PR is waiting on them instead of silently
                    // letting it squat the queue.
                    items.push(serde_json::json!({
                        "kind": "review_pr", "number": pr.number,
                        "title": pr.title, "url": pr.url,
                        "held": true,
                        "role": "SA/dev", "can_act": my_role.can_review(),
                    }));
                }
            }
        }
    }
    Json(serde_json::json!({ "user": me, "items": items })).into_response()
}

/// GET `/api/projects/:pid/attachment?key=…` — stream one attachment's bytes
/// from blob storage (MinIO/S3 or the local blob dir) with its content type.
pub(super) async fn attachment_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let key = params.get("key").cloned().unwrap_or_default();
    if key.is_empty() || key.contains("..") {
        return (axum::http::StatusCode::BAD_REQUEST, "bad key").into_response();
    }
    let Some(storage) = &p.storage else {
        return (
            axum::http::StatusCode::CONFLICT,
            "no blob storage configured",
        )
            .into_response();
    };
    // The record on the ticket is the authority for the content type; fall
    // back to octet-stream for keys nothing references (e.g. pruned tickets).
    let ct = p
        .store
        .load()
        .await
        .ok()
        .and_then(|s| {
            s.ticket_attachments
                .values()
                .flatten()
                .find(|a| a.key == key)
                .map(|a| a.content_type.clone())
        })
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    match storage.get(&key).await {
        Ok(bytes) => (
            [
                (axum::http::header::CONTENT_TYPE, ct),
                // Never let a stored SVG/HTML run script in the dashboard origin.
                (
                    axum::http::header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; style-src 'unsafe-inline'".to_owned(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => (axum::http::StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

/// POST `/api/projects/:pid/ticket/:id/attachments?name=…` — a person uploads
/// an attachment (raw bytes body, `Content-Type` header carries the MIME).
pub(super) async fn upload_attachment_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let me = principal_name(&app, &headers)
        .await
        .unwrap_or_else(|| "operator".to_owned());
    let name = params.get("name").cloned().unwrap_or_default();
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    let name = safe(&name);
    if name.is_empty() || body.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "name query param and a non-empty body are required",
        )
            .into_response();
    }
    let ct = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let Some(storage) = &p.storage else {
        return (
            axum::http::StatusCode::CONFLICT,
            "no blob storage configured",
        )
            .into_response();
    };
    let key = format!("uploads/{}/{}", safe(&id), name);
    if let Err(e) = storage.put(&key, &body, &ct).await {
        return internal_error(&e.to_string());
    }
    let rec = coxagent_application::state::TicketAttachment {
        name,
        key: key.clone(),
        content_type: ct,
        by: me,
        at: coxagent_application::state::now_rfc3339(),
    };
    let saved = coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        s.ticket_attachments
            .entry(id.clone())
            .or_default()
            .push(rec.clone());
        Ok(())
    })
    .await;
    match saved {
        Ok(()) => Json(serde_json::json!({ "ok": true, "attachment": rec })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// POST `/api/projects/:pid/pr/:number/human` — a person decides a PR the
/// machine held for human eyes. `{"action":"approve"}` lands it via the forge
/// (this IS the human the gate waited for); `{"action":"dismiss"}` clears the
/// Inbox entry and leaves the PR for handling on the forge itself.
pub(super) async fn human_pr_ep(
    State(app): State<AppState>,
    Path((pid, number)): Path<(String, u64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<HumanActionReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(me) = gate_principal(&app, &headers, coxagent_application::AuthRole::can_review).await
    else {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response();
    };
    match req.action.as_str() {
        "approve" => {
            let Some(forge) = &p.forge else {
                return (
                    axum::http::StatusCode::CONFLICT,
                    "this hub has no forge access — merge it on the forge directly",
                )
                    .into_response();
            };
            if let Err(e) = forge.merge_pr(number).await {
                return (
                    axum::http::StatusCode::BAD_GATEWAY,
                    format!("merge failed: {e}"),
                )
                    .into_response();
            }
        }
        "dismiss" => {}
        other => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                format!("action must be approve or dismiss, got {other}"),
            )
                .into_response()
        }
    }
    let verb = if req.action == "approve" {
        "landed"
    } else {
        "dismissed the hold on"
    };
    let note = format!("🧑‍⚖️ @{me} {verb} PR #{number} (held for human eyes).");
    // Governance-attention ledger (CXA-F230): recorded inside the same atomic
    // mutation, and ONLY when this call actually resolved the hold — the
    // second of two concurrent resolutions finds nothing to remove and counts
    // no effort (AC5).
    let kind = if req.action == "approve" {
        coxagent_domain::InterventionKind::HumanPrReviewed
    } else {
        coxagent_domain::InterventionKind::HumanPrDismissed
    };
    if coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), move |s| {
        if s.human_holds.remove(&number).is_some() {
            s.record_pr_intervention(kind, number, &me);
        }
        s.log_activity("USER", &format!("{me} {verb} PR #{number}"), None);
        s.post_comment(&me, &note, None);
        Ok(())
    })
    .await
    .is_err()
    {
        return internal_error("store write failed");
    }
    Json(serde_json::json!({ "ok": true, "action": req.action })).into_response()
}

/// The one-field body every human one-click decision endpoint takes: which
/// of the two verdicts the person chose ("approve" / "dismiss").
#[derive(serde::Deserialize)]
pub(super) struct HumanActionReq {
    #[serde(default)]
    pub(super) action: String,
}

/// POST `/api/projects/:pid/reverts/:sha` — a person decides one detected
/// revert (CXA-F047). `{"action":"approve"}` confirms the shipped work really
/// was undone — the only verdict next-cycle planning may learn from;
/// `{"action":"dismiss"}` records it as a false positive. Already-decided
/// events are final, so a double submit cannot flip a verdict.
pub(super) async fn revert_decision_ep(
    State(app): State<AppState>,
    Path((pid, sha)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<HumanActionReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(me) = gate_principal(&app, &headers, coxagent_application::AuthRole::can_review).await
    else {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response();
    };
    let decision = match req.action.as_str() {
        "approve" => coxagent_application::state::RevertDecision::Approved,
        "dismiss" => coxagent_application::state::RevertDecision::Dismissed,
        other => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                format!("action must be approve or dismiss, got {other}"),
            )
                .into_response()
        }
    };
    let verb = if req.action == "approve" {
        "approved"
    } else {
        "dismissed"
    };
    let sha = sha.trim().to_owned();
    if sha.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "missing sha").into_response();
    }
    let mut decided = false;
    let mut ticket = String::new();
    if coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        ticket = s
            .reverted_work
            .iter()
            .find(|e| e.sha == sha)
            .map(|e| e.ticket.clone())
            .unwrap_or_default();
        decided = s.decide_revert(&sha, decision, &me);
        if decided {
            s.log_activity(
                "USER",
                &format!("{me} {verb} reverted work {ticket}"),
                Some(ticket.clone()),
            );
        }
        Ok(())
    })
    .await
    .is_err()
    {
        return internal_error("store write failed");
    }
    if !decided {
        return (
            axum::http::StatusCode::CONFLICT,
            "no pending revert with that sha",
        )
            .into_response();
    }
    Json(serde_json::json!({ "ok": true, "action": req.action })).into_response()
}

/// POST `/api/projects/:pid/ticket/:id/ready` — a person approves a designed
/// ticket into `Ready` (the hybrid PO gate).
pub(super) async fn human_ready_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    human_transition(
        &app,
        &pid,
        &id,
        &headers,
        coxagent_domain::Status::Ready,
        coxagent_application::AuthRole::can_approve_ready,
    )
    .await
}

/// POST `/api/projects/:pid/ticket/:id/verify` — a person renders the QA
/// verdict on a fixed ticket (the hybrid verify gate).
pub(super) async fn human_verify_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    human_transition(
        &app,
        &pid,
        &id,
        &headers,
        coxagent_domain::Status::Verified,
        coxagent_application::AuthRole::can_verify,
    )
    .await
}

/// POST `/api/projects/:pid/ticket/:id/send-back` — the verify gate's other
/// answer: this fix is not demonstrated, do it again.
///
/// Approving was the only button. A reviewer who found no evidence could
/// comment "there is no evidence" and watch nothing happen: the ticket stayed
/// in `Fixed`, out of the dev queue, waiting for a verdict the reviewer had
/// already reached. The reason travels with it as a comment, which is what
/// steers the next attempt.
pub(super) async fn send_back_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: Option<Json<super::work::RejectReq>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(me) = gate_principal(&app, &headers, coxagent_application::AuthRole::can_verify).await
    else {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response();
    };
    let reason = body.map(|Json(r)| r.reason).unwrap_or_default();
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(t) = state.tickets.iter_mut().find(|t| t.id().as_str() == id) else {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    };
    if let Err(e) = t.transition_to(coxagent_domain::Role::User, coxagent_domain::Status::Open) {
        return (axum::http::StatusCode::CONFLICT, e.to_string()).into_response();
    }
    let note = if reason.trim().is_empty() {
        format!("↩️ {id} sent back by @{me}: the fix is not demonstrated.")
    } else {
        format!("↩️ {id} sent back by @{me}: {}", reason.trim())
    };
    state.log_activity("USER", "verification refused", Some(id.clone()));
    state.post_comment(&me, &note, Some(id.clone()));
    // Governance-attention ledger (CXA-F230): a send-back is re-review churn
    // the operator paid for — the exact signal the ledger exists to surface.
    // The transition above is guarded, so a duplicate submit is a 409 here.
    state.record_intervention(coxagent_domain::InterventionKind::VerifySendBack, &id, &me);
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn human_transition(
    app: &AppState,
    pid: &str,
    id: &str,
    headers: &axum::http::HeaderMap,
    to: coxagent_domain::Status,
    allowed: fn(coxagent_application::AuthRole) -> bool,
) -> axum::response::Response {
    let Some(p) = app.project(pid).await else {
        return not_found();
    };
    // The gate exists to put a QUALIFIED person in front of the decision. Any
    // signed-in account could take it before — including a Viewer, whose whole
    // definition is read-only.
    let Some(me) = gate_principal(app, headers, allowed).await else {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response();
    };
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(t) = state.tickets.iter_mut().find(|t| t.id().as_str() == id) else {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    };
    if let Err(e) = t.transition_to(coxagent_domain::Role::User, to) {
        return (axum::http::StatusCode::CONFLICT, e.to_string()).into_response();
    }
    // Reaching Verified MEANS the human QA verdict was rendered; the burn-down
    // (CXA-F032 AC#2) only counts bugs carrying their own REGRESSION TEST PASS
    // record, so this path writes the same provenance the agent TEST path has
    // written since F022.
    if to == coxagent_domain::Status::Verified {
        coxagent_application::use_cases::run_test::record_human_verify_evidence(
            &mut state,
            id,
            &me,
        );
        // Goal-line outcome ledger (CXA-F228): a human verdict is a delivered
        // outcome like the agent path's.
        state.record_verified_outcome(id);
    }
    // Governance-attention ledger (CXA-F230): the verdict just taken is a
    // measured moment of operator review effort, attributed to the ticket's
    // class. The transition guards above make a duplicate submit a 409 before
    // any record exists, so one resolution is one record. Explicitly mapped
    // per target status — a future caller adding a third target must decide
    // what kind it is, never silently inherit ReadyApprove.
    let kind = match to {
        coxagent_domain::Status::Verified => Some(coxagent_domain::InterventionKind::VerifyPass),
        coxagent_domain::Status::Ready => Some(coxagent_domain::InterventionKind::ReadyApprove),
        _ => None,
    };
    if let Some(kind) = kind {
        state.record_intervention(kind, id, &me);
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
    state.log_activity(
        "USER",
        &format!("{me} moved ticket to {label}"),
        Some(id.to_owned()),
    );
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
    // Undo reverses an approval and retires the learned rule behind it — the
    // same weight as approving, so the same qualification.
    let Some(me) = gate_principal(
        &app,
        &headers,
        coxagent_application::AuthRole::can_approve_ready,
    )
    .await
    else {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response();
    };
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
    if let Err(e) = t.transition_to(
        coxagent_domain::Role::User,
        coxagent_domain::Status::Pending,
    ) {
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
    // Governance-attention ledger (CXA-F230): an undo is the strongest form of
    // gate friction the ledger tracks. The window membership check above
    // refuses a second undo with a 409, so one pull-back is one record.
    state.record_intervention(coxagent_domain::InterventionKind::UndoAutoApprove, &id, &me);
    let note = format!(
        "↩️ @{me} undid the auto-approval of {id} — `{shape}` goes back to asking a person."
    );
    state.log_activity("USER", "undid an auto-approval", Some(id.clone()));
    state.post_chat_in(
        "SYSTEM",
        &note,
        coxagent_application::state::APPROVALS_CHANNEL,
        Vec::new(),
    );
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// DELETE `/api/projects/:pid/ticket/:id/attachments?key=…` — remove one
/// attachment record from the ticket and best-effort delete its blob.
pub(super) async fn delete_attachment_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let key = params.get("key").cloned().unwrap_or_default();
    if key.is_empty() || key.contains("..") {
        return (axum::http::StatusCode::BAD_REQUEST, "bad key").into_response();
    }
    let removed = coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        if let Some(list) = s.ticket_attachments.get_mut(&id) {
            list.retain(|a| a.key != key);
            if list.is_empty() {
                s.ticket_attachments.remove(&id);
            }
        }
        Ok(())
    })
    .await;
    if let Err(e) = removed {
        return internal_error(&e.to_string());
    }
    // The record is the authority for what a ticket shows; the blob itself is
    // left in storage (StoragePort has no delete) and is harmless stranded.
    Json(serde_json::json!({ "ok": true })).into_response()
}
