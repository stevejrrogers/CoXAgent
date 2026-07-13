//! HTTP server (inbound adapter) — a multi-project hub. Serves the embedded
//! dashboard plus a per-project JSON API, SSE stream, controllable runner, and
//! ticket actions. A single-project `serve` registers one project; `hub`
//! registers many. Routes are scoped `/api/projects/:pid/...`.

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use coxagent_application::metrics;
use coxagent_application::ports::outbound::StateStorePort;
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
}

/// Builds a fresh project on demand (scaffold + register), injected by the
/// composition root so the presentation layer stays free of infrastructure.
/// Takes `(name, alias)`, returns a ready [`ProjectHandle`] or an error message.
pub type ProjectFactory = Arc<
    dyn Fn(String, Option<String>) -> Pin<Box<dyn Future<Output = Result<ProjectHandle, String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct AppState {
    projects: Arc<RwLock<HashMap<String, ProjectHandle>>>,
    order: Arc<RwLock<Vec<String>>>,
    factory: Option<ProjectFactory>,
}

impl AppState {
    /// Clone out the handle for a project id (cheap — all fields are `Arc`).
    async fn project(&self, pid: &str) -> Option<ProjectHandle> {
        self.projects.read().await.get(pid).cloned()
    }
}

/// Serve the dashboard and API for the given projects on `port`.
///
/// # Errors
/// Returns an IO error if the port cannot be bound.
pub async fn serve(projects: Vec<ProjectHandle>, port: u16) -> std::io::Result<()> {
    serve_with_factory(projects, port, None).await
}

/// Serve with a [`ProjectFactory`] so the dashboard can onboard new projects at
/// runtime (POST `/api/projects`). `serve` passes `None` (static project set).
///
/// # Errors
/// Returns an IO error if the port cannot be bound.
pub async fn serve_with_factory(
    projects: Vec<ProjectHandle>,
    port: u16,
    factory: Option<ProjectFactory>,
) -> std::io::Result<()> {
    let order: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();
    let map: HashMap<String, ProjectHandle> =
        projects.into_iter().map(|p| (p.id.clone(), p)).collect();
    let state = AppState {
        projects: Arc::new(RwLock::new(map)),
        order: Arc::new(RwLock::new(order)),
        factory,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/:pid/state", get(state_ep))
        .route("/api/projects/:pid/metrics", get(metrics_ep))
        .route("/api/projects/:pid/runner", get(runner_ep))
        .route("/api/projects/:pid/audit", get(audit_ep))
        .route("/api/projects/:pid/config", get(get_config).put(put_config))
        .route("/api/projects/:pid/control/:action", post(control_ep))
        .route("/api/projects/:pid/ticket/:id/priority", post(set_priority))
        .route("/api/projects/:pid/ticket/:id/reject", post(reject_ticket))
        .route(
            "/api/projects/:pid/comments",
            get(list_comments).post(post_comment),
        )
        .route("/api/projects/:pid/events", get(events_ep))
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
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
            return (axum::http::StatusCode::CONFLICT, "project id already exists").into_response();
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
    let stream = IntervalStream::new(tokio::time::interval(STREAM_INTERVAL)).then(move |_| {
        let handle = handle.clone();
        async move {
            let payload = match handle {
                Some(p) => {
                    let state = p.store.load().await.ok();
                    serde_json::json!({ "state": state, "runner": p.runner.snapshot() })
                }
                None => serde_json::json!({ "error": "no such project" }),
            };
            Ok(Event::default().data(payload.to_string()))
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
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
