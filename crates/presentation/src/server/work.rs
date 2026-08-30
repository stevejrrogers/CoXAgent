// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The work surface: tickets, sprints, and the runner controls.

use super::*;

/// Refine a rough project goal into a project brief (goal, stack, scope,
/// constraints) via the hub engine — for review before creating the project.
pub(super) async fn analyze_goal_ep(
    State(app): State<AppState>,
    Json(req): Json<GoalReq>,
) -> axum::response::Response {
    use coxagent_application::ports::outbound::AgentRequest;
    let Some((engine, work_dir)) = app.analyzer.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "no engine configured").into_response();
    };
    let goal = req.goal.trim();
    if goal.is_empty() {
        return (StatusCode::BAD_REQUEST, "goal is required").into_response();
    }
    let request = AgentRequest {
        role: coxagent_domain::Role::Ba,
        system_prompt: "You are the BA and PO of a software team scoping a NEW project. \
            Turn the stakeholder's rough goal into a crisp project brief."
            .to_owned(),
        task_prompt: format!(
            "Rough goal:\n{goal}\n\nWrite a concise project brief in markdown with EXACTLY these \
             sections and nothing else:\n## Goal\n(what we're building, for whom, the problem)\n\
             ## Tech stack\n(frontend / backend / database / infra)\n## Product scope\n(feature \
             groups the BA may propose)\n## Constraints\n(auth, deploy target, performance)"
        ),
        work_dir,
        timeout: std::time::Duration::from_secs(120),
        escalation_level: 0,
        label: None,
    };
    match engine.run(request).await {
        Ok(o) if o.succeeded() => {
            Json(serde_json::json!({ "brief": o.stdout.trim() })).into_response()
        }
        Ok(o) => internal_error(&format!("engine failed: {}", o.stderr.trim())),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Full detail for one ticket — including the `design` specs stripped from list
/// payloads — loaded only when the user opens it.
pub(super) async fn ticket_detail_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match p.store.load().await {
        Ok(state) => state
            .tickets
            .iter()
            .find(|t| t.id().as_str() == id)
            .map_or_else(not_found, |t| {
                let mut v = serde_json::to_value(t).unwrap_or_default();
                // Cost-gate surface: the hold estimate (if any) and whether a
                // human already approved this ticket to run.
                if let Some(obj) = v.as_object_mut() {
                    // Test-to-AC traceability (CXA-F024): the Test Coverage tab
                    // renders straight from the aggregate's computed matrix —
                    // the status logic stays in the domain, not in the view.
                    obj.insert(
                        "coverage_matrix".into(),
                        serde_json::to_value(t.coverage_matrix()).unwrap_or_default(),
                    );
                    if let Some(est) = state.cost_holds.get(&id) {
                        obj.insert("cost_hold".into(), serde_json::json!(est));
                    }
                    if let Some(ev) = state.ticket_evidence.get(&id) {
                        obj.insert(
                            "evidence".into(),
                            serde_json::to_value(ev).unwrap_or_default(),
                        );
                    }
                    if state.cost_approved.contains(&id) {
                        obj.insert("cost_approved".into(), serde_json::json!(true));
                    }
                    // Attachments ride the detail payload: the modal renders
                    // from here, always fresh — the SSE snapshot path proved
                    // unreliable as a source (records reached the store but
                    // never the client's STATE).
                    if let Some(atts) = state.ticket_attachments.get(&id) {
                        obj.insert(
                            "attachments".into(),
                            serde_json::to_value(atts).unwrap_or_default(),
                        );
                    }
                    // Dependency radar (CXA-F237): why this ticket is not
                    // running — direct blockers with their LIVE statuses
                    // (AC1), and every depends_on id absent from the project
                    // state surfaced as unknown, never treated as satisfied
                    // (AC3). Pure derivation over the same loaded snapshot.
                    obj.insert(
                        "blocked_by".into(),
                        serde_json::to_value(coxagent_application::dependency_radar::blocked_by(
                            &state,
                            t.id(),
                        ))
                        .unwrap_or_default(),
                    );
                    let unknown_pairs =
                        coxagent_application::dependency_radar::unknown_dependencies(&state);
                    let unknown: Vec<&coxagent_domain::TicketId> = unknown_pairs
                        .iter()
                        .filter(|(dep, _)| dep == t.id())
                        .map(|(_, missing)| missing)
                        .collect();
                    if !unknown.is_empty() {
                        obj.insert(
                            "unknown_dependencies".into(),
                            serde_json::to_value(unknown).unwrap_or_default(),
                        );
                    }
                }
                Json(v).into_response()
            }),
        Err(e) => internal_error(&e.to_string()),
    }
}

pub(super) async fn runner_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    Json(p.runner.snapshot()).into_response()
}

/// Facilitate a multi-agent discussion on a topic: PO and SA weigh in, SM
/// decides and may create a ticket. Turns are posted to the team channel.
pub(super) async fn run_discussion_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<DiscussReq>,
) -> axum::response::Response {
    use coxagent_application::use_cases::RunDiscussionUseCase;
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let topic = req.topic.trim();
    if topic.is_empty() {
        return (StatusCode::BAD_REQUEST, "topic is required").into_response();
    }
    let uc = RunDiscussionUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
    )
    .with_language(project_language(&p));
    match uc.execute(topic).await {
        Ok(o) => Json(serde_json::json!({
            "ok": true, "turns": o.turns, "decision": o.decision,
            "created_ticket": o.created_ticket,
        }))
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Run the BA agent on a rough idea and return a refined ticket proposal for
/// the user to review — WITHOUT saving. The user edits and saves via the normal
/// create-ticket endpoint. Needs a working engine (claude/opencode).
pub(super) async fn ba_analyze(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<AnalyzeReq>,
) -> axum::response::Response {
    use coxagent_application::parsing::parse_items;
    use coxagent_application::ports::outbound::AgentRequest;
    use coxagent_application::prompts;
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let idea = req.description.trim();
    if idea.is_empty() {
        return (StatusCode::BAD_REQUEST, "description is required").into_response();
    }
    let request = AgentRequest {
        role: coxagent_domain::Role::Ba,
        system_prompt: prompts::system_prompt(prompts::BA),
        task_prompt: format!(
            "A stakeholder proposes this idea. Refine it into ONE well-formed \
             feature ticket (crisp title, clear description, sensible priority / \
             complexity / has_ui). Respond with the same JSON array shape, one item:\n\n{idea}"
        ),
        work_dir: p.work_dir.clone(),
        timeout: std::time::Duration::from_secs(120),
        escalation_level: 0,
        label: None,
    };
    let outcome = match p.engine.run(request).await {
        Ok(o) if o.succeeded() => o,
        Ok(o) => return internal_error(&format!("BA engine failed: {}", o.stderr.trim())),
        Err(e) => return internal_error(&e.to_string()),
    };
    match parse_items(&outcome.stdout) {
        Ok(items) if !items.is_empty() => {
            let p0 = &items[0];
            Json(serde_json::json!({
                "title": p0.title, "description": p0.description,
                "priority": p0.priority, "complexity": p0.complexity, "has_ui": p0.has_ui,
            }))
            .into_response()
        }
        Ok(_) => (StatusCode::UNPROCESSABLE_ENTITY, "BA returned no proposal").into_response(),
        Err(e) => internal_error(&format!("could not parse BA output: {e}")),
    }
}

/// Collaborative refine: PO/SA/PD advise, then the BA synthesises a polished,
/// build-ready ticket for the user to review. Nothing is saved.
pub(super) async fn ticket_refine(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<AnalyzeReq>,
) -> axum::response::Response {
    use coxagent_application::use_cases::RefineTicketUseCase;
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let idea = req.description.trim();
    if idea.is_empty() {
        return (StatusCode::BAD_REQUEST, "description is required").into_response();
    }
    let context = tokio::fs::read_to_string(&p.context_path)
        .await
        .unwrap_or_default();
    let uc = RefineTicketUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
    )
    .with_token_saver(project_token_saver(&p));
    match uc.execute(idea, &context).await {
        Ok(t) => Json(t).into_response(),
        Err(e) => internal_error(&format!("ticket refine failed: {e}")),
    }
}

/// Create a ticket in the backlog (the manual entry point; the BA/SA/DEV
/// pipeline then designs and builds it, highest priority first).
pub(super) async fn create_ticket(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<CreateTicketReq>,
) -> axum::response::Response {
    use coxagent_application::use_cases::{AddTicketInput, AddTicketUseCase};
    use coxagent_domain::{Complexity, Priority, TicketType};
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let title = req.title.trim();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "title is required").into_response();
    }
    let ticket_type = match req.ticket_type.as_deref() {
        Some("bug") => TicketType::Bug,
        Some("chore") => TicketType::Chore,
        _ => TicketType::Feature,
    };
    // Declared product goal (CXA-F228): blank/absent means none; a non-blank
    // id is parsed strictly so a malformed association is a 400, never a
    // silently unattributed ticket.
    let goal = match req.goal.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(g) => match coxagent_domain::GoalId::new(g) {
            Ok(gid) => Some(gid),
            Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        },
    };
    let input = AddTicketInput {
        ticket_type,
        title: title.to_owned(),
        description: req.description.trim().to_owned(),
        priority: req.priority.unwrap_or(Priority::Medium),
        complexity: req.complexity.unwrap_or(Complexity::Medium),
        has_ui: req.has_ui,
        acceptance_criteria: req.acceptance_criteria.clone(),
        goal,
    };
    match AddTicketUseCase::new(Arc::clone(&p.store))
        .execute(input)
        .await
    {
        Ok(id) => {
            if let Ok(mut state) = p.store.load().await {
                state.log_activity("USER", "created ticket", Some(id.to_string()));
                let _ = p.store.save(&state).await;
            }
            Json(serde_json::json!({ "ok": true, "id": id.to_string() })).into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Un-park a ticket: clear its fail-attempt counter and journal so agents
/// pick it up again — the human's "this deserves another shot" button.
pub(super) async fn unpark_ticket(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let had = state.ticket_fail_attempts.remove(&id).is_some();
    state.ticket_journal.remove(&id);
    // Also reset the merged-PR sync memory: if this ticket's fix already
    // merged, the next forge-hygiene pass will close it properly instead of
    // agents retrying a landed fix. Idempotent for everything else.
    state.seen_merged_prs.clear();
    if !had && !state.tickets.iter().any(|t| t.id().as_str() == id) {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    }
    state.log_activity("USER", "un-parked ticket", Some(id.clone()));
    state.post_comment(
        "SM",
        &format!("▶️ {id} un-parked by a human — agents may retry it."),
        Some(id),
    );
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Human approval for a cost-held ticket: clears the hold and whitelists the
/// ticket so the DEV gate lets it run despite the estimate.
pub(super) async fn approve_cost(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Attribution only — the gate decision itself stays open to whoever could
    // always take it; the ledger records WHO paid attention, it does not start
    // refusing decisions.
    let me = super::guards::principal_name(&app, &headers)
        .await
        .unwrap_or_else(|| "operator".to_owned());
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    if state.cost_holds.remove(&id).is_none()
        && !state.tickets.iter().any(|t| t.id().as_str() == id)
    {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    }
    // Governance-attention ledger (CXA-F230): record the FIRST approval only —
    // re-approving an already-approved ticket is a no-op repeat, and counting
    // it again would inflate the operator's attributed effort (AC5).
    let first_approval = !state.cost_approved.contains(&id);
    state.cost_approved.insert(id.clone());
    if first_approval {
        state.record_intervention(coxagent_domain::InterventionKind::CostApprove, &id, &me);
    }
    state.log_activity("USER", "approved cost", Some(id));
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Edit a ticket's title + description (scope owner action).
pub(super) async fn edit_ticket(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    Json(req): Json<EditReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(tid) = coxagent_domain::TicketId::new(id.clone()) else {
        return (axum::http::StatusCode::BAD_REQUEST, "bad id").into_response();
    };
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(ticket) = state.ticket_mut(&tid) else {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    };
    if let Err(e) = ticket.edit(coxagent_domain::Role::User, req.title, req.description) {
        return (axum::http::StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    state.log_activity("USER", "edited ticket", Some(id));
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

pub(super) async fn reject_ticket(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: Option<Json<RejectReq>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(tid) = coxagent_domain::TicketId::new(id.clone()) else {
        return (axum::http::StatusCode::BAD_REQUEST, "bad id").into_response();
    };
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    // A rejection REASON is the most valuable thing a human types: it becomes
    // a pre-flight check so the same shape never reaches an inbox again
    // (docs/ADAPTIVE_APPROVAL.md).
    let reason = body.map(|Json(r)| r.reason).unwrap_or_default();
    // Rejecting is the same gate decision as approving, taken the other way —
    // and it teaches the learner, so it needs the same qualification.
    let Some(me) = super::inbox::gate_principal(
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
    if let Some(t) = state.tickets.iter().find(|t| t.id() == &tid) {
        let shape = coxagent_application::use_cases::approval_risk::shape_key(t);
        state.approval_samples.push(
            coxagent_application::use_cases::approval_memory::ApprovalSample {
                shape,
                decision: "reject".to_owned(),
                by: me.clone(),
                reason: reason.trim().to_owned(),
                at: coxagent_application::state::now_rfc3339(),
            },
        );
    }
    if !reason.trim().is_empty() {
        state.post_comment(
            "USER",
            &format!("🚫 Rejected: {}", reason.trim()),
            Some(id.clone()),
        );
    }
    let Some(ticket) = state.ticket_mut(&tid) else {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    };
    if let Err(e) = ticket.transition_to(
        coxagent_domain::Role::User,
        coxagent_domain::Status::Rejected,
    ) {
        return (axum::http::StatusCode::CONFLICT, e.to_string()).into_response();
    }
    // A rejected ticket is not awaiting work — drop any cost hold so it stops
    // showing as a spend to approve.
    state.cost_holds.remove(&id);
    state.cost_approved.remove(&id);
    state.log_activity("USER", "rejected ticket", Some(id));
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Extract the body of the `## Goal` markdown section (empty if absent).
pub(super) fn extract_goal(md: &str) -> String {
    let mut in_goal = false;
    let mut buf: Vec<&str> = Vec::new();
    for line in md.lines() {
        if line.starts_with("## ") {
            in_goal = line.trim() == "## Goal";
            continue;
        }
        if in_goal {
            buf.push(line);
        }
    }
    buf.join("\n").trim().to_owned()
}

/// Rewrite the `## Goal` section's body, preserving the rest of the brief.
/// Prepends a `## Goal` section when none exists.
pub(super) fn replace_goal(md: &str, goal: &str) -> String {
    let mut out = String::new();
    let mut in_goal = false;
    let mut wrote = false;
    for line in md.lines() {
        if line.starts_with("## ") {
            if line.trim() == "## Goal" {
                in_goal = true;
                out.push_str("## Goal\n");
                out.push_str(goal.trim());
                out.push('\n');
                wrote = true;
                continue;
            }
            in_goal = false;
        }
        if in_goal {
            continue; // drop the old goal body until the next header
        }
        out.push_str(line);
        out.push('\n');
    }
    if wrote {
        out
    } else {
        format!("## Goal\n{}\n\n{}", goal.trim(), md)
    }
}

/// SM-run standup: posts a deterministic status roundup to the team channel and
/// pulls in each agent's latest contribution. Zero engine cost — derived from
/// state — so it can be triggered freely to see the team "gather".
/// Current sprint number (0 when not in a sprint), for review labels.
pub(super) async fn current_sprint(p: &ProjectHandle) -> u32 {
    p.store
        .load()
        .await
        .ok()
        .and_then(|s| s.sprint.map(|sp| sp.number))
        .unwrap_or(0)
}

#[allow(clippy::too_many_lines)] // one linear pass; splitting hurts readability
pub(super) async fn standup_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    use coxagent_domain::ticket::{Status, TicketType};
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(mut s) = p.store.load().await else {
        return internal_error("load failed");
    };
    let vi = project_language(&p).is_vi();

    let inflight = s
        .tickets
        .iter()
        .filter(|t| matches!(t.status(), Status::Ready | Status::InProgress))
        .count();
    let shipped = s
        .tickets
        .iter()
        .filter(|t| matches!(t.status(), Status::Done | Status::Documented))
        .count();
    let blockers: Vec<String> = s
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
        .map(|t| t.id().to_string())
        .collect();

    let header = if let Some(sp) = &s.sprint {
        let done = sp
            .committed
            .iter()
            .filter(|id| {
                s.tickets.iter().any(|t| {
                    t.id() == *id && matches!(t.status(), Status::Done | Status::Documented)
                })
            })
            .count();
        if vi {
            format!(
                "Standup — Sprint #{} \u{201c}{}\u{201d}: {}/{} cam kết đã ship, {inflight} đang làm, {} blocker.",
                sp.number, sp.goal, done, sp.committed.len(), blockers.len()
            )
        } else {
            format!(
                "Standup — Sprint #{} \u{201c}{}\u{201d}: {}/{} committed shipped, {inflight} in flight, {} blocker(s).",
                sp.number, sp.goal, done, sp.committed.len(), blockers.len()
            )
        }
    } else if vi {
        format!(
            "Standup — {shipped} đã ship, {inflight} đang làm, {} blocker.",
            blockers.len()
        )
    } else {
        format!(
            "Standup — {shipped} shipped, {inflight} in flight, {} blocker(s).",
            blockers.len()
        )
    };
    s.post_comment("SM", &header, None);

    // Pull each agent into the standup with its latest recorded contribution.
    for agent in ["BA", "SA", "PD", "DEV-FEATURE", "DEV-BUG", "TEST", "DOCS"] {
        if let Some(act) = s.activity.iter().rev().find(|e| e.agent == agent) {
            let tk = act
                .ticket
                .as_deref()
                .map_or_else(String::new, |t| format!(" ({t})"));
            let line = format!("{}{tk}.", act.action);
            s.post_comment(agent, &line, None);
        }
    }

    let closing = if blockers.is_empty() {
        if vi {
            "Focus: dồn sức cho backlog sprint — ship xong rồi hãy đề xuất thêm.".to_owned()
        } else {
            "Focus: keep burning the sprint backlog — ship before proposing more.".to_owned()
        }
    } else {
        let ids = blockers
            .iter()
            .take(3)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if vi {
            format!(
                "Focus: dọn {} bug đang mở trước ({ids}). DEV-BUG ưu tiên mấy cái này hơn tính năng.",
                blockers.len()
            )
        } else {
            format!(
                "Focus: clear {} open bug(s) first ({ids}). DEV-BUG, these take priority over features.",
                blockers.len()
            )
        }
    };
    s.post_comment("SM", &closing, None);

    match p.store.save(&s).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

pub(super) async fn control_ep(
    State(app): State<AppState>,
    Path((pid, action)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Attribute the run to whoever started it, on this machine, so the agent
    // cards can show "account@host" and claims are owned correctly. A headless
    // worker (no login session) takes its identity from COXAGENT_OPERATOR — the
    // way a `coxagent run` box on another machine gets a distinct name.
    let account = match std::env::var("COXAGENT_OPERATOR") {
        Ok(o) if !o.is_empty() => o,
        _ => resolve_username(&app, &headers).await,
    };
    let operator = format!("{account}@{}", machine_host());
    // Ownership gate: a run belongs to whoever started it. Only that user — or
    // an admin/root — may pause, stop, or step it. Anyone else pressing Start
    // while someone's run is live only records THEIR desired-run intent (their
    // own operator picks it up); it never hijacks or relabels the live run.
    let (owner, live) = {
        let s = p.runner.snapshot();
        (s.operator, s.mode == "running")
    };
    let owns = match app.auth.clone() {
        None => true, // open mode: single-user local
        Some(auth) => {
            let caller = resolve_principal(&auth, &headers).await;
            caller.as_ref().is_some_and(|u| {
                matches!(
                    u.role,
                    coxagent_application::auth::AuthRole::Super
                        | coxagent_application::auth::AuthRole::Admin
                ) || owner
                    .as_deref()
                    .map_or(true, |o| o.eq_ignore_ascii_case(&u.username))
            })
        }
    };
    match action.as_str() {
        "resume" => {
            if live && !owns {
                // Someone else's run is live: just start MY operator.
                let _ = p.store.set_desired(&operator, true).await;
                return Json(p.runner.snapshot()).into_response();
            }
            p.runner.set_operator(&account, &machine_host());
            p.runner.resume();
            // Persist this operator's intent so reopening the app auto-resumes
            // for THIS user only — never starts anyone else's operator.
            let _ = p.store.set_desired(&operator, true).await;
        }
        // Pause/stop are local to this operator and persist the stopped intent,
        // so a reopen stays idle instead of auto-resuming.
        "pause" | "step" | "stop" if !owns => {
            let who = owner.unwrap_or_default();
            return (
                axum::http::StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": format!("this run belongs to {who} — only they or an admin can {action} it")
                })),
            )
                .into_response();
        }
        "pause" => {
            p.runner.pause();
            let _ = p.store.set_desired(&operator, false).await;
        }
        "step" => p.runner.step(),
        "stop" => {
            p.runner.stop();
            let _ = p.store.set_desired(&operator, false).await;
        }
        other => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("unknown action {other}") })),
            )
                .into_response()
        }
    }
    Json(p.runner.snapshot()).into_response()
}

/// Control any operator (by `account@host`) from the dashboard: set its desired
/// run state, which that operator honours on its next cycle — so Stop reaches a
/// worker on another machine (or a headless one) without touching processes.
/// Stopping only idles it (saves its credentials); Start requires the operator's
/// process to be alive and waiting.
pub(super) async fn operator_control_ep(
    State(app): State<AppState>,
    Path((pid, operator, action)): Path<(String, String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Each user controls only their OWN team: the operator's account (the part
    // before `@`) must match the logged-in user — unless they're an admin, who
    // may manage everyone. Open mode (no auth) allows it (single-user local).
    if let Some(auth) = app.auth.clone() {
        let caller = resolve_principal(&auth, &headers).await;
        let account = operator.split('@').next().unwrap_or("");
        // Admin/root manage everyone; leads and below only their own operator.
        let allowed = caller.as_ref().is_some_and(|u| {
            matches!(
                u.role,
                coxagent_application::auth::AuthRole::Super
                    | coxagent_application::auth::AuthRole::Admin
            ) || u.username.eq_ignore_ascii_case(account)
        });
        if !allowed {
            return (
                axum::http::StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "you can only control your own operator" })),
            )
                .into_response();
        }
    }
    let running = match action.as_str() {
        "start" => true,
        "stop" => false,
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "action must be start or stop" })),
            )
                .into_response()
        }
    };
    match p.store.set_desired(&operator, running).await {
        Ok(()) => Json(serde_json::json!({ "ok": true, "operator": operator, "running": running }))
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Set the PO's goal for the upcoming sprint. It becomes the sprint goal on the
/// next roll-over and steers the BA to propose tickets that advance it — the
/// proposals still pass the normal Pending→Ready refinement gate before any DEV
/// work, so nothing is built without vetting.
pub(super) async fn set_sprint_goal_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<SprintGoalReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let goal = req.goal.trim().to_owned();
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        s.sprint_goal.clone_from(&goal);
        Ok(())
    })
    .await
    {
        Ok(()) => Json(serde_json::json!({ "ok": true, "goal": goal })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Human burn mode (CXA-F030): pause feature work and burn down open bugs
/// until the count reaches the exit gate. `target: null` sets no numeric
/// gate — the mode then holds until switched off with `enabled: false`.
pub(super) async fn burn_mode_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<BurnModeReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        s.tuning.burn_mode = req.enabled;
        s.tuning.burn_until_bugs_le = req.target;
        Ok(())
    })
    .await
    {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "burn_mode": req.enabled,
            "target": req.target.unwrap_or(0),
        }))
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Pull tickets into the sprint that is already running, or drop them from it.
///
/// The automatic commit is capacity-based and happens once, at roll-over. A
/// person deciding mid-sprint that something belongs in it (or no longer does)
/// had no way to say so — the scope was whatever the machine picked.
pub(super) async fn sprint_scope_ep(
    State(app): State<AppState>,
    Path((pid, action)): Path<(String, String)>,
    Json(req): Json<SprintScopeReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let adding = match action.as_str() {
        "commit" => true,
        "drop" => false,
        _ => return (StatusCode::BAD_REQUEST, "action must be commit or drop").into_response(),
    };
    let ids: Vec<coxagent_domain::TicketId> = req
        .tickets
        .iter()
        .filter_map(|t| coxagent_domain::TicketId::new(t.trim()).ok())
        .collect();
    if ids.is_empty() {
        return (StatusCode::BAD_REQUEST, "no valid ticket ids").into_response();
    }
    let mut changed = 0usize;
    // Ids the board has never heard of: a typo, or a stale page acting on a
    // ticket that has since gone. Saying "ok" to that hides the mistake.
    let mut unknown: Vec<String> = Vec::new();
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        for id in &ids {
            if s.ticket(id).is_none() {
                unknown.push(id.to_string());
                continue;
            }
            let hit = if adding {
                coxagent_application::sprint::commit_ticket(s, id)
            } else {
                coxagent_application::sprint::uncommit_ticket(s, id)
            };
            if hit {
                changed += 1;
            }
        }
        Ok(())
    })
    .await
    {
        Ok(()) if changed == 0 && !unknown.is_empty() => (
            StatusCode::BAD_REQUEST,
            format!("no such ticket: {}", unknown.join(", ")),
        )
            .into_response(),
        Ok(()) => Json(serde_json::json!({ "ok": true, "changed": changed, "unknown": unknown }))
            .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Park a ticket (`on_hold`) or resume it. Holding is a person's process call
/// for work blocked on the outside world (a billing account, a vendor): the
/// ticket stays on the board but sprint auto-commit, refill and agent pickup
/// all skip it — unlike Rejected, it comes back with one click.
pub(super) async fn hold_ticket_ep(
    State(app): State<AppState>,
    Path((pid, id, action)): Path<(String, String, String)>,
    headers: axum::http::HeaderMap,
    body: Option<Json<super::HoldReq>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(tid) = coxagent_domain::TicketId::new(id.clone()) else {
        return (StatusCode::BAD_REQUEST, "bad id").into_response();
    };
    let holding = match action.as_str() {
        "hold" => true,
        "resume" => false,
        _ => return (StatusCode::BAD_REQUEST, "action must be hold or resume").into_response(),
    };
    // Same qualification as reject: this is a person's gate decision.
    let Some(me) = super::inbox::gate_principal(
        &app,
        &headers,
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
    let mut err: Option<String> = None;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        let Some(t) = s.tickets.iter_mut().find(|t| t.id() == &tid) else {
            err = Some("no such ticket".to_owned());
            return Ok(());
        };
        let to = if holding {
            coxagent_domain::Status::OnHold
        } else {
            match t.ticket_type() {
                coxagent_domain::TicketType::Bug => coxagent_domain::Status::Open,
                _ => coxagent_domain::Status::Pending,
            }
        };
        if let Err(e) = t.transition_to(coxagent_domain::Role::User, to) {
            err = Some(e.to_string());
            return Ok(());
        }
        if holding {
            let reason = body
                .as_ref()
                .map(|Json(r)| r.reason.trim().to_owned())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "held by a person".to_owned());
            s.hold_reasons.insert(tid.to_string(), reason);
        } else {
            // Resume forgives the failure history (and the hold reason) —
            // otherwise the auto-hold sweep would park it right back.
            coxagent_application::sprint::clear_fail_attempts(s, &tid);
        }
        s.log_activity(
            &me,
            if holding {
                "put on hold"
            } else {
                "resumed from hold"
            },
            Some(tid.to_string()),
        );
        Ok(())
    })
    .await
    {
        Ok(()) => match err {
            Some(e) => (StatusCode::BAD_REQUEST, e).into_response(),
            None => Json(serde_json::json!({ "ok": true })).into_response(),
        },
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Queue a sprint to run after the current one. The queue is consumed
/// front-first at roll-over: the plan's goal and ticket set become the next
/// sprint's. Planning is additive — an empty queue changes nothing.
pub(super) async fn queue_sprint_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<super::QueueSprintReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let goal = req.goal.trim().to_owned();
    if goal.is_empty() {
        return (StatusCode::BAD_REQUEST, "goal must not be empty").into_response();
    }
    let by = resolve_username(&app, &headers).await;
    let ids: Vec<coxagent_domain::TicketId> = req
        .tickets
        .iter()
        .filter_map(|t| coxagent_domain::TicketId::new(t.trim()).ok())
        .collect();
    let mut qid = 0u64;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        qid = coxagent_application::sprint::queue_sprint(s, &goal, ids.clone(), &by);
        Ok(())
    })
    .await
    {
        Ok(()) => Json(serde_json::json!({ "ok": true, "id": qid })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Add/remove tickets on one queued sprint.
pub(super) async fn queue_scope_ep(
    State(app): State<AppState>,
    Path((pid, qid)): Path<(String, u64)>,
    Json(req): Json<super::QueueScopeReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let parse = |v: &[String]| -> Vec<coxagent_domain::TicketId> {
        v.iter()
            .filter_map(|t| coxagent_domain::TicketId::new(t.trim()).ok())
            .collect()
    };
    let (add, remove) = (parse(&req.add), parse(&req.remove));
    let mut hit = false;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        hit = coxagent_application::sprint::scope_queued_sprint(s, qid, &add, &remove);
        Ok(())
    })
    .await
    {
        Ok(()) if !hit => (StatusCode::NOT_FOUND, "no such queued sprint").into_response(),
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Rename a queued sprint's goal.
pub(super) async fn queue_rename_ep(
    State(app): State<AppState>,
    Path((pid, qid)): Path<(String, u64)>,
    Json(req): Json<super::SprintGoalReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    if req.goal.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "goal must not be empty").into_response();
    }
    let mut hit = false;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        hit = coxagent_application::sprint::rename_queued_sprint(s, qid, &req.goal);
        Ok(())
    })
    .await
    {
        Ok(()) if !hit => (StatusCode::NOT_FOUND, "no such queued sprint").into_response(),
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Move a queued sprint up or down in run order.
pub(super) async fn queue_move_ep(
    State(app): State<AppState>,
    Path((pid, qid, dir)): Path<(String, u64, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let delta: i64 = match dir.as_str() {
        "up" => -1,
        "down" => 1,
        _ => return (StatusCode::BAD_REQUEST, "dir must be up or down").into_response(),
    };
    let mut hit = false;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        hit = coxagent_application::sprint::move_queued_sprint(s, qid, delta);
        Ok(())
    })
    .await
    {
        Ok(()) if !hit => (
            StatusCode::BAD_REQUEST,
            "no such queued sprint, or already at that end",
        )
            .into_response(),
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Drop a queued sprint outright.
pub(super) async fn queue_delete_ep(
    State(app): State<AppState>,
    Path((pid, qid)): Path<(String, u64)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let mut hit = false;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        hit = coxagent_application::sprint::delete_queued_sprint(s, qid);
        Ok(())
    })
    .await
    {
        Ok(()) if !hit => (StatusCode::NOT_FOUND, "no such queued sprint").into_response(),
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Close the running sprint NOW and open the next one, instead of waiting for
/// the window to elapse. The closed sprint is archived exactly as a timed
/// roll-over archives it, so the velocity history stays one shape.
pub(super) async fn sprint_close_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let cycle = p.runner.snapshot().cycle;
    let mut opened = None;
    match coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        opened = coxagent_application::sprint::close_now(s, cycle);
        Ok(())
    })
    .await
    {
        Ok(()) => match opened {
            Some(n) => Json(serde_json::json!({ "ok": true, "sprint": n })).into_response(),
            None => (StatusCode::BAD_REQUEST, "no sprint is running").into_response(),
        },
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Optional body for a rejection: the reason, which teaches the gate.
#[derive(serde::Deserialize, Default)]
pub(super) struct RejectReq {
    #[serde(default)]
    pub(super) reason: String,
}
