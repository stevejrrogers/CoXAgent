//! HTTP server (inbound adapter) — a multi-project hub. Serves the embedded
//! dashboard plus a per-project JSON API, SSE stream, controllable runner, and
//! ticket actions. A single-project `serve` registers one project; `hub`
//! registers many. Routes are scoped `/api/projects/:pid/...`.

use axum::extract::{Path, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use coxagent_application::auth::AuthPort;
use coxagent_application::metrics;
use coxagent_application::ports::outbound::{AuditPort, AuditRecord, StateStorePort};
use coxagent_application::use_cases::RunnerHandle;
use coxagent_application::Config;
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::{Stream, StreamExt};

/// The embedded single-page dashboard.
const INDEX_HTML: &str = include_str!("web/index.html");

/// How often the SSE stream pushes a fresh snapshot.
const STREAM_INTERVAL: Duration = Duration::from_secs(1);

/// One managed project the hub serves.
#[derive(Clone)]
pub struct ProjectHandle {
    pub id: String,
    pub name: String,
    pub alias: String,
    pub store: Arc<dyn StateStorePort>,
    pub runner: Arc<RunnerHandle>,
    pub config_path: PathBuf,
    /// The engine, exposed for on-demand actions (e.g. BA idea analysis).
    pub engine: Arc<dyn coxagent_application::ports::outbound::AgentEnginePort>,
    /// The managed codebase directory.
    pub work_dir: PathBuf,
}

/// Builds a fresh project on demand (scaffold + register), injected by the
/// composition root so the presentation layer stays free of infrastructure.
/// Takes `(name, alias)`, returns a ready [`ProjectHandle`] or an error message.
pub type ProjectFactory = Arc<
    dyn Fn(
            String,
            Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<ProjectHandle, String>> + Send>>
        + Send
        + Sync,
>;

/// Record one audit entry through the injected sink (fire-and-forget).
async fn audit_push(sink: &Arc<dyn AuditPort>, user: &str, action: String, status: u16) {
    sink.record(AuditRecord {
        at: now_rfc3339(),
        user: user.to_owned(),
        action,
        status,
    })
    .await;
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[derive(Clone)]
struct AppState {
    projects: Arc<RwLock<HashMap<String, ProjectHandle>>>,
    order: Arc<RwLock<Vec<String>>>,
    factory: Option<ProjectFactory>,
    auth: Option<Arc<dyn AuthPort>>,
    audit: Arc<dyn AuditPort>,
    /// Agent CLIs detected on this machine's PATH: `(name, path)`.
    engines: Arc<Vec<(String, String)>>,
    /// Live dashboard connections (desktop window + browser tabs share the hub).
    viewers: Arc<std::sync::atomic::AtomicUsize>,
}

/// Increments the live-viewer count for its lifetime; decrements on drop when
/// the SSE stream ends (tab closed / window quit).
struct ViewerGuard(Arc<std::sync::atomic::AtomicUsize>);

impl ViewerGuard {
    fn new(counter: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(Arc::clone(counter))
    }
    /// Current viewer count. Also keeps the guard owned by the SSE closure.
    fn count(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl AppState {
    /// Clone out the handle for a project id (cheap — all fields are `Arc`).
    async fn project(&self, pid: &str) -> Option<ProjectHandle> {
        self.projects.read().await.get(pid).cloned()
    }
}

/// Serve the dashboard and API. `factory` enables dashboard onboarding; `auth`
/// (when it has users) enforces RBAC; `audit` is the security-audit sink.
///
/// # Errors
/// Returns an IO error if the port cannot be bound.
pub async fn serve_full(
    projects: Vec<ProjectHandle>,
    port: u16,
    factory: Option<ProjectFactory>,
    audit: Arc<dyn AuditPort>,
    auth: Option<Arc<dyn AuthPort>>,
    engines: Vec<(String, String)>,
) -> std::io::Result<()> {
    let order: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();
    let map: HashMap<String, ProjectHandle> =
        projects.into_iter().map(|p| (p.id.clone(), p)).collect();
    let state = AppState {
        projects: Arc::new(RwLock::new(map)),
        order: Arc::new(RwLock::new(order)),
        factory,
        auth,
        audit,
        engines: Arc::new(engines),
        viewers: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/auth/login", post(login_ep))
        .route("/api/auth/logout", post(logout_ep))
        .route("/api/auth/me", get(me_ep))
        .route(
            "/api/auth/tokens",
            get(list_tokens_ep).post(create_token_ep),
        )
        .route(
            "/api/auth/tokens/:label",
            axum::routing::delete(revoke_token_ep),
        )
        .route("/api/auth/2fa/enroll", post(enroll_2fa_ep))
        .route("/api/auth/2fa/enable", post(enable_2fa_ep))
        .route("/api/auth/2fa/disable", post(disable_2fa_ep))
        .route("/api/auth/users", get(list_users_ep).post(create_user_ep))
        .route(
            "/api/auth/users/:username",
            axum::routing::delete(delete_user_ep),
        )
        .route("/api/audit-log", get(audit_log_ep))
        .route("/api/engines", get(engines_ep))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/:pid/state", get(state_ep))
        .route("/api/projects/:pid/metrics", get(metrics_ep))
        .route("/api/projects/:pid/runner", get(runner_ep))
        .route("/api/projects/:pid/audit", get(audit_ep))
        .route("/api/projects/:pid/config", get(get_config).put(put_config))
        .route("/api/projects/:pid/control/:action", post(control_ep))
        .route("/api/projects/:pid/ba-analyze", post(ba_analyze))
        .route("/api/projects/:pid/discuss", post(run_discussion_ep))
        .route("/api/projects/:pid/tickets", post(create_ticket))
        .route("/api/projects/:pid/ticket/:id/priority", post(set_priority))
        .route("/api/projects/:pid/ticket/:id/reject", post(reject_ticket))
        .route(
            "/api/projects/:pid/comments",
            get(list_comments).post(post_comment),
        )
        .route("/api/projects/:pid/transcripts", get(list_transcripts))
        .route("/api/projects/:pid/transcripts/:name", get(get_transcript))
        .route("/api/projects/:pid/events", get(events_ep))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state);

    // Bind loopback by default (safe for local use); a container sets
    // COXAGENT_HOST=0.0.0.0 so published ports are reachable from the host.
    let host = std::env::var("COXAGENT_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("dashboard on http://{addr}");
    axum::serve(listener, app).await
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Agent CLIs detected on this machine's PATH — so the dashboard can show what
/// can actually run locally, not just the known engine types.
async fn engines_ep(State(app): State<AppState>) -> impl IntoResponse {
    let list: Vec<_> = app
        .engines
        .iter()
        .map(|(name, path)| serde_json::json!({ "name": name, "path": path }))
        .collect();
    Json(list)
}

/// List projects (id, name, alias, version, ticket count) in registration order.
async fn list_projects(State(app): State<AppState>) -> impl IntoResponse {
    let order = app.order.read().await.clone();
    let mut out = Vec::new();
    for id in &order {
        if let Some(p) = app.project(id).await {
            let (version, tickets) = p.store.load().await.map_or_else(
                |_| ("0.0.0".to_owned(), 0),
                |s| (s.current_version.to_string(), s.tickets.len()),
            );
            out.push(serde_json::json!({
                "id": p.id, "name": p.name, "alias": p.alias,
                "version": version, "tickets": tickets,
                "mode": p.runner.snapshot().mode,
            }));
        }
    }
    Json(out)
}

#[derive(serde::Deserialize)]
struct CreateProjectReq {
    name: String,
    #[serde(default)]
    alias: Option<String>,
}

/// Onboard a new project from the dashboard: scaffold its workspace via the
/// injected factory and register it live. Returns the new project's id.
async fn create_project(
    State(app): State<AppState>,
    Json(req): Json<CreateProjectReq>,
) -> axum::response::Response {
    let Some(factory) = app.factory.clone() else {
        return (
            axum::http::StatusCode::NOT_IMPLEMENTED,
            "onboarding is only available in hub mode",
        )
            .into_response();
    };
    let name = req.name.trim().to_owned();
    if name.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    let handle = match factory(name, req.alias).await {
        Ok(h) => h,
        Err(e) => return internal_error(&e),
    };
    let id = handle.id.clone();
    {
        let mut map = app.projects.write().await;
        if map.contains_key(&id) {
            return (
                axum::http::StatusCode::CONFLICT,
                "project id already exists",
            )
                .into_response();
        }
        map.insert(id.clone(), handle);
        app.order.write().await.push(id.clone());
    }
    Json(serde_json::json!({ "ok": true, "id": id })).into_response()
}

async fn state_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match p.store.load().await {
        Ok(state) => Json(serde_json::to_value(state).unwrap_or_default()).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn metrics_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match p.store.load().await {
        Ok(state) => Json(metrics::compute(&state)).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn runner_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    Json(p.runner.snapshot()).into_response()
}

async fn audit_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let entries = p.store.load().await.map(|s| s.activity).unwrap_or_default();
    let body = serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_owned());
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"coxagent-audit.json\"",
            ),
        ],
        body,
    )
        .into_response()
}

async fn get_config(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    Json(cfg).into_response()
}

async fn put_config(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(cfg): Json<Config>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match serde_json::to_string_pretty(&cfg) {
        Ok(text) => match std::fs::write(&p.config_path, text) {
            Ok(()) => {
                Json(serde_json::json!({ "ok": true, "note": "restart to apply" })).into_response()
            }
            Err(e) => internal_error(&e.to_string()),
        },
        Err(e) => internal_error(&e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct PriorityReq {
    priority: coxagent_domain::Priority,
}

#[derive(serde::Deserialize)]
struct CreateTicketReq {
    #[serde(default)]
    ticket_type: Option<String>,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    priority: Option<coxagent_domain::Priority>,
    #[serde(default)]
    complexity: Option<coxagent_domain::Complexity>,
    #[serde(default)]
    has_ui: bool,
}

#[derive(serde::Deserialize)]
struct DiscussReq {
    topic: String,
}

/// Facilitate a multi-agent discussion on a topic: PO and SA weigh in, SM
/// decides and may create a ticket. Turns are posted to the team channel.
async fn run_discussion_ep(
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
    );
    match uc.execute(topic).await {
        Ok(o) => Json(serde_json::json!({
            "ok": true, "turns": o.turns, "decision": o.decision,
            "created_ticket": o.created_ticket,
        }))
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct AnalyzeReq {
    description: String,
}

/// Run the BA agent on a rough idea and return a refined ticket proposal for
/// the user to review — WITHOUT saving. The user edits and saves via the normal
/// create-ticket endpoint. Needs a working engine (claude/opencode).
async fn ba_analyze(
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

/// Create a ticket in the backlog (the manual entry point; the BA/SA/DEV
/// pipeline then designs and builds it, highest priority first).
async fn create_ticket(
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
    let input = AddTicketInput {
        ticket_type,
        title: title.to_owned(),
        description: req.description.trim().to_owned(),
        priority: req.priority.unwrap_or(Priority::Medium),
        complexity: req.complexity.unwrap_or(Complexity::Medium),
        has_ui: req.has_ui,
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

async fn set_priority(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    Json(req): Json<PriorityReq>,
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
    if let Err(e) = ticket.set_priority(coxagent_domain::Role::User, req.priority) {
        return (axum::http::StatusCode::FORBIDDEN, e.to_string()).into_response();
    }
    state.log_activity("USER", "set priority", Some(id));
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn reject_ticket(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
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
    if let Err(e) = ticket.transition_to(
        coxagent_domain::Role::User,
        coxagent_domain::Status::Rejected,
    ) {
        return (axum::http::StatusCode::CONFLICT, e.to_string()).into_response();
    }
    state.log_activity("USER", "rejected ticket", Some(id));
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// List discussion comments, optionally filtered to one ticket via `?ticket=ID`.
async fn list_comments(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<CommentQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let mut comments = p.store.load().await.map(|s| s.comments).unwrap_or_default();
    if let Some(tid) = q.ticket {
        comments.retain(|c| c.ticket.as_deref() == Some(tid.as_str()));
    }
    Json(comments).into_response()
}

#[derive(serde::Deserialize)]
struct CommentQuery {
    ticket: Option<String>,
}

#[derive(serde::Deserialize)]
struct PostCommentReq {
    body: String,
    #[serde(default)]
    ticket: Option<String>,
}

/// Post a comment (as the user) to a ticket thread or the team channel.
async fn post_comment(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<PostCommentReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let body = req.body.trim();
    if body.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "empty comment").into_response();
    }
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    state.post_comment("USER", body, req.ticket);
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// The transcript directory for a project: `<workspace>/logs/transcripts`.
fn transcripts_dir(p: &ProjectHandle) -> PathBuf {
    p.config_path
        .parent()
        .unwrap_or(&p.config_path)
        .join("logs")
        .join("transcripts")
}

/// List transcript files (name + size + modified), newest first.
async fn list_transcripts(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let dir = transcripts_dir(&p);
    let mut items: Vec<serde_json::Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = entries.flatten().collect();
        files.sort_by_key(|e| {
            std::cmp::Reverse(
                e.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            )
        });
        for e in files.into_iter().take(200) {
            let name = e.file_name().to_string_lossy().into_owned();
            let size = e.metadata().map_or(0, |m| m.len());
            items.push(serde_json::json!({ "name": name, "size": size }));
        }
    }
    Json(items).into_response()
}

/// Return one transcript's content. The name is validated to prevent traversal.
async fn get_transcript(
    State(app): State<AppState>,
    Path((pid, name)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Reject any path separators / traversal — only a bare filename is allowed.
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let path = transcripts_dir(&p).join(&name);
    match std::fs::read_to_string(&path) {
        Ok(body) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such transcript").into_response(),
    }
}

async fn control_ep(
    State(app): State<AppState>,
    Path((pid, action)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match action.as_str() {
        "resume" => p.runner.resume(),
        "pause" => p.runner.pause(),
        "step" => p.runner.step(),
        "stop" => p.runner.stop(),
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

async fn events_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let handle = app.project(&pid).await;
    let guard = ViewerGuard::new(&app.viewers);
    let stream = IntervalStream::new(tokio::time::interval(STREAM_INTERVAL)).then(move |_| {
        // `guard` is owned by this closure, so the count drops when the stream ends.
        let count = guard.count();
        let handle = handle.clone();
        async move {
            let payload = match handle {
                Some(p) => serde_json::json!({
                    "state": p.store.load().await.ok(),
                    "runner": p.runner.snapshot(),
                    "viewers": count,
                }),
                None => serde_json::json!({ "error": "no such project" }),
            };
            Ok(Event::default().data(payload.to_string()))
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Name of the session cookie.
const SESSION_COOKIE: &str = "cox_session";

/// Extract a cookie value from a `Cookie` header set.
fn cookie_value(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == name).then(|| v.trim().to_owned())
    })
}

/// The `Authorization: Bearer <token>` value, if present.
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ").map(|t| t.trim().to_owned())
}

/// Resolve the principal: an `Authorization: Bearer` API token (for automation)
/// takes precedence, else the session cookie.
async fn resolve_principal(
    auth: &Arc<dyn AuthPort>,
    headers: &axum::http::HeaderMap,
) -> Option<coxagent_application::AuthUser> {
    if let Some(token) = bearer_token(headers) {
        if let Some(user) = auth.principal_for_bearer(&token).await {
            return Some(user);
        }
    }
    match cookie_value(headers, SESSION_COOKIE) {
        Some(token) => auth.user_for(&token).await,
        None => None,
    }
}

/// RBAC gate. Open (pass-through) when no auth is configured. Otherwise: the
/// SPA shell, health, and login are public; every other route needs a valid
/// session, and mutating methods (except logout) need an admin.
async fn auth_mw(
    State(app): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return next.run(req).await;
    };
    let path = req.uri().path().to_owned();
    if path == "/" || path == "/api/health" || path == "/api/auth/login" {
        return next.run(req).await;
    }
    let Some(user) = resolve_principal(&auth, req.headers()).await else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthenticated" })),
        )
            .into_response();
    };
    let is_write = matches!(
        *req.method(),
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::DELETE
            | axum::http::Method::PATCH
    ) && path != "/api/auth/logout"
        && !path.starts_with("/api/auth/2fa/"); // self-service, any signed-in user
    let method = req.method().clone();
    let username = user.username.clone();
    if is_write && !user.role.can_write() {
        audit_push(
            &app.audit,
            &username,
            format!("{method} {path}"),
            StatusCode::FORBIDDEN.as_u16(),
        )
        .await;
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "admin role required" })),
        )
            .into_response();
    }
    let resp = next.run(req).await;
    // Record every authenticated mutation with its outcome.
    if is_write {
        audit_push(
            &app.audit,
            &username,
            format!("{method} {path}"),
            resp.status().as_u16(),
        )
        .await;
    }
    resp
}

#[derive(serde::Deserialize)]
struct CreateTokenReq {
    label: String,
    /// "admin" or "viewer" (defaults to viewer).
    #[serde(default)]
    role: Option<String>,
}

/// Mint an API token for a service account. The secret is returned once and
/// never stored in plaintext. Admin-only (enforced by the middleware).
async fn create_token_ep(
    State(app): State<AppState>,
    Json(req): Json<CreateTokenReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let label = req.label.trim();
    if label.is_empty() {
        return (StatusCode::BAD_REQUEST, "label is required").into_response();
    }
    let role = match req.role.as_deref() {
        Some("admin") => coxagent_application::AuthRole::Admin,
        _ => coxagent_application::AuthRole::Viewer,
    };
    match auth.create_token(label, role).await {
        Some(secret) => Json(serde_json::json!({
            "ok": true, "label": label, "token": secret,
            "note": "store this now — it is not shown again",
        }))
        .into_response(),
        None => (StatusCode::CONFLICT, "label already in use").into_response(),
    }
}

/// List minted API tokens (metadata only). Admin-only via middleware write gate
/// is not applied to GET, so restrict to admins explicitly.
async fn list_tokens_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(Vec::<coxagent_application::TokenInfo>::new()).into_response();
    };
    let is_admin = resolve_principal(&auth, &headers)
        .await
        .is_some_and(|u| u.role.can_write());
    if !is_admin {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    Json(auth.list_tokens().await).into_response()
}

/// Revoke an API token by label. Admin-only (write gate in middleware).
async fn revoke_token_ep(
    State(app): State<AppState>,
    Path(label): Path<String>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if auth.revoke_token(&label).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such token").into_response()
    }
}

#[derive(serde::Deserialize)]
struct CreateUserReq {
    username: String,
    password: String,
    #[serde(default)]
    role: Option<String>,
}

fn role_from(s: Option<&str>) -> coxagent_application::AuthRole {
    match s {
        Some("admin") => coxagent_application::AuthRole::Admin,
        _ => coxagent_application::AuthRole::Viewer,
    }
}

/// List user accounts (admin-only). Returns `[{username, role}]`.
async fn list_users_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(Vec::<coxagent_application::AuthUser>::new()).into_response();
    };
    let is_admin = resolve_principal(&auth, &headers)
        .await
        .is_some_and(|u| u.role.can_write());
    if !is_admin {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    Json(auth.list_users().await).into_response()
}

/// Create or update a user account (admin-only via the write gate).
async fn create_user_ep(
    State(app): State<AppState>,
    Json(req): Json<CreateUserReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if req.username.trim().is_empty() || req.password.is_empty() {
        return (StatusCode::BAD_REQUEST, "username and password required").into_response();
    }
    if auth
        .create_user(
            req.username.trim(),
            &req.password,
            role_from(req.role.as_deref()),
        )
        .await
    {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        internal_error("could not create user")
    }
}

/// Delete a user account (admin-only). Refuses to remove the last admin.
async fn delete_user_ep(
    State(app): State<AppState>,
    Path(username): Path<String>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if auth.delete_user(&username).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (
            StatusCode::CONFLICT,
            "cannot delete (unknown user or last admin)",
        )
            .into_response()
    }
}

/// Return the security audit log (admin only, newest first).
async fn audit_log_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    // When auth is on, require an admin; open mode exposes it freely.
    if let Some(auth) = app.auth.clone() {
        let ok = match resolve_principal(&auth, &headers).await {
            Some(u) => u.role.can_write(),
            None => false,
        };
        if !ok {
            return (StatusCode::FORBIDDEN, "admin role required").into_response();
        }
    }
    match app.audit.recent(1000).await {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Begin 2FA enrollment for the signed-in user: returns the secret + otpauth URI.
async fn enroll_2fa_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
    };
    match auth.enroll_2fa(&user.username).await {
        Some((secret, uri)) => {
            Json(serde_json::json!({ "secret": secret, "uri": uri })).into_response()
        }
        None => internal_error("could not start enrollment"),
    }
}

#[derive(serde::Deserialize)]
struct CodeReq {
    code: String,
}

/// Activate 2FA for the signed-in user after confirming a code.
async fn enable_2fa_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CodeReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
    };
    if auth.enable_2fa(&user.username, req.code.trim()).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::BAD_REQUEST, "invalid or expired code").into_response()
    }
}

/// Disable 2FA for the signed-in user.
async fn disable_2fa_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
    };
    auth.disable_2fa(&user.username).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[derive(serde::Deserialize)]
struct LoginReq {
    username: String,
    password: String,
    /// TOTP code, required when the account has 2FA enabled.
    #[serde(default)]
    totp: Option<String>,
}

/// Verify credentials (and 2FA when enabled) and set an HttpOnly session cookie.
async fn login_ep(
    State(app): State<AppState>,
    Json(req): Json<LoginReq>,
) -> axum::response::Response {
    use coxagent_application::LoginResult;
    let Some(auth) = app.auth.clone() else {
        return Json(serde_json::json!({ "ok": true, "auth": false })).into_response();
    };
    let token = match auth
        .login(&req.username, &req.password, req.totp.as_deref())
        .await
    {
        LoginResult::Ok(token) => token,
        LoginResult::TotpRequired => {
            // Password is correct; the client must supply a 2FA code next.
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "totp required", "totp_required": true })),
            )
                .into_response();
        }
        LoginResult::Denied => {
            audit_push(
                &app.audit,
                &req.username,
                "failed login".to_owned(),
                StatusCode::UNAUTHORIZED.as_u16(),
            )
            .await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "invalid credentials" })),
            )
                .into_response();
        }
    };
    let user = auth.user_for(&token).await;
    let role = user.as_ref().map_or("viewer", |u| {
        if u.role.can_write() {
            "admin"
        } else {
            "viewer"
        }
    });
    audit_push(&app.audit, &req.username, "login".to_owned(), 200).await;
    let cookie =
        format!("{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200");
    (
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({ "ok": true, "username": req.username, "role": role })),
    )
        .into_response()
}

/// Invalidate the session and clear the cookie.
async fn logout_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let (Some(auth), Some(token)) = (app.auth.clone(), cookie_value(&headers, SESSION_COOKIE)) {
        auth.logout(&token).await;
    }
    let cleared = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    (
        [(header::SET_COOKIE, cleared)],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

/// Report the current principal (or `auth:false` when running open).
async fn me_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(serde_json::json!({ "auth": false })).into_response();
    };
    match resolve_principal(&auth, &headers).await {
        Some(u) => {
            let twofa = auth.has_2fa(&u.username).await;
            Json(serde_json::json!({
                "auth": true, "username": u.username,
                "role": if u.role.can_write() { "admin" } else { "viewer" },
                "twofa": twofa,
            }))
            .into_response()
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "auth": true })),
        )
            .into_response(),
    }
}

fn not_found() -> axum::response::Response {
    (axum::http::StatusCode::NOT_FOUND, "no such project").into_response()
}

fn internal_error(msg: &str) -> axum::response::Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}
