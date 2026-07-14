//! HTTP server (inbound adapter) — a multi-project hub. Serves the embedded
//! dashboard plus a per-project JSON API, SSE stream, controllable runner, and
//! ticket actions. A single-project `serve` registers one project; `hub`
//! registers many. Routes are scoped `/api/projects/:pid/...`.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
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
    /// Live spend caps shared with the running loop, so budget edits apply now.
    pub budget: coxagent_application::LiveBudget,
    /// The `project_context.md` brief (goal + tech stack) agents are seeded with.
    pub context_path: PathBuf,
}

/// Builds a fresh project on demand (scaffold + register), injected by the
/// composition root so the presentation layer stays free of infrastructure.
/// Takes `(name, alias)`, returns a ready [`ProjectHandle`] or an error message.
pub type ProjectFactory = Arc<
    dyn Fn(NewProjectReq) -> Pin<Box<dyn Future<Output = Result<ProjectHandle, String>> + Send>>
        + Send
        + Sync,
>;

/// A request to create a project. `existing` adopts a codebase (brownfield);
/// `goal` seeds the project context (from AI-assisted goal drafting).
#[derive(Clone, Default)]
pub struct NewProjectReq {
    pub name: String,
    pub alias: Option<String>,
    pub existing: Option<PathBuf>,
    pub goal: Option<String>,
}

/// Deregisters a project (removes it from the hub registry), injected by the
/// composition root. Returns an error message on failure.
pub type ProjectRemover =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

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

/// A project's live team-chat channel: a broadcast fan-out to every connected
/// WebSocket, plus a mutex that serializes the load→append→save of chat writes
/// so two simultaneous messages can't clobber each other.
#[derive(Clone)]
struct ChatChannel {
    tx: tokio::sync::broadcast::Sender<String>,
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone)]
struct AppState {
    projects: Arc<RwLock<HashMap<String, ProjectHandle>>>,
    /// Per-project team-chat channels, created lazily on first use.
    chat_bus: Arc<RwLock<HashMap<String, ChatChannel>>>,
    order: Arc<RwLock<Vec<String>>>,
    factory: Option<ProjectFactory>,
    auth: Option<Arc<dyn AuthPort>>,
    audit: Arc<dyn AuditPort>,
    /// Agent CLIs detected on this machine's PATH: `(name, path)`.
    engines: Arc<Vec<(String, String)>>,
    /// Live viewers keyed by username → open-connection count. Distinct users =
    /// map length, so one person in the app + a browser tab counts once.
    viewers: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    /// Deregisters a project from the hub registry.
    remover: Option<ProjectRemover>,
    /// A hub-level engine for cross-project drafting (e.g. project goals), with a
    /// working directory to run it in.
    analyzer: Option<(
        Arc<dyn coxagent_application::ports::outbound::AgentEnginePort>,
        PathBuf,
    )>,
}

/// Registers one live connection for a user; on drop (stream closed) it
/// deregisters, so distinct-user counts stay accurate across tabs.
type Viewers = Arc<std::sync::Mutex<HashMap<String, usize>>>;
struct ViewerGuard {
    viewers: Viewers,
    user: String,
}

impl ViewerGuard {
    fn new(viewers: &Viewers, user: String) -> Self {
        if let Ok(mut map) = viewers.lock() {
            *map.entry(user.clone()).or_insert(0) += 1;
        }
        Self {
            viewers: Arc::clone(viewers),
            user,
        }
    }
    /// Number of distinct users currently connected. Also keeps the guard owned
    /// by the SSE closure.
    fn count(&self) -> usize {
        self.viewers.lock().map_or(1, |m| m.len().max(1))
    }
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.viewers.lock() {
            if let Some(n) = map.get_mut(&self.user) {
                *n -= 1;
                if *n == 0 {
                    map.remove(&self.user);
                }
            }
        }
    }
}

impl AppState {
    /// Clone out the handle for a project id (cheap — all fields are `Arc`).
    async fn project(&self, pid: &str) -> Option<ProjectHandle> {
        self.projects.read().await.get(pid).cloned()
    }

    /// Get (or lazily create) the live chat channel for a project.
    async fn chat_channel(&self, pid: &str) -> ChatChannel {
        if let Some(ch) = self.chat_bus.read().await.get(pid) {
            return ch.clone();
        }
        let mut bus = self.chat_bus.write().await;
        bus.entry(pid.to_owned())
            .or_insert_with(|| ChatChannel {
                tx: tokio::sync::broadcast::channel(256).0,
                write_lock: Arc::new(tokio::sync::Mutex::new(())),
            })
            .clone()
    }
}

/// Persist one chat message and fan it out to every live WebSocket. The
/// per-project `write_lock` serializes the load→append→save so concurrent
/// senders can't lose each other's messages. Returns `false` if persistence
/// fails. `body` must already be validated (non-empty, length-capped).
async fn deliver_chat(app: &AppState, p: &ProjectHandle, user: &str, body: &str) -> bool {
    let ch = app.chat_channel(&p.id).await;
    let _guard = ch.write_lock.lock().await;
    let Ok(mut state) = p.store.load().await else {
        return false;
    };
    state.post_chat(user, body);
    let msg = state.chat.last().cloned();
    if p.store.save(&state).await.is_err() {
        return false;
    }
    if let Some(m) = msg {
        // Ignore send errors: a broadcast with no live receivers is fine.
        let _ = ch.tx.send(serde_json::to_string(&m).unwrap_or_default());
    }
    true
}

/// Optional hub capabilities injected by the composition root, keeping the
/// presentation layer free of infrastructure.
#[derive(Default)]
pub struct HubExtras {
    /// Onboard a new project at runtime (dashboard "New project").
    pub factory: Option<ProjectFactory>,
    /// Deregister a project (dashboard "Delete project").
    pub remover: Option<ProjectRemover>,
    /// RBAC (login required when it has users).
    pub auth: Option<Arc<dyn AuthPort>>,
    /// Agent CLIs detected on this machine's PATH: `(name, path)`.
    pub engines: Vec<(String, String)>,
    /// A hub-level engine + work dir for cross-project drafting (project goals).
    pub analyzer: Option<(
        Arc<dyn coxagent_application::ports::outbound::AgentEnginePort>,
        PathBuf,
    )>,
}

/// Serve the dashboard and API on `port`, with the security-audit sink and the
/// optional hub capabilities in `extras`.
///
/// # Errors
/// Returns an IO error if the port cannot be bound.
pub async fn serve_full(
    projects: Vec<ProjectHandle>,
    port: u16,
    audit: Arc<dyn AuditPort>,
    extras: HubExtras,
) -> std::io::Result<()> {
    let order: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();
    let map: HashMap<String, ProjectHandle> =
        projects.into_iter().map(|p| (p.id.clone(), p)).collect();
    let state = AppState {
        projects: Arc::new(RwLock::new(map)),
        chat_bus: Arc::new(RwLock::new(HashMap::new())),
        order: Arc::new(RwLock::new(order)),
        factory: extras.factory,
        auth: extras.auth,
        audit,
        engines: Arc::new(extras.engines),
        viewers: Arc::new(std::sync::Mutex::new(HashMap::new())),
        remover: extras.remover,
        analyzer: extras.analyzer,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/auth/login", post(login_ep))
        .route("/api/auth/logout", post(logout_ep))
        .route("/api/auth/me", get(me_ep))
        .route("/api/auth/sessions", get(sessions_ep))
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
        .route("/api/people-analytics", get(people_analytics_ep))
        .route("/api/projects/:pid/workspace", get(workspace_ep))
        .route("/api/projects/:pid/file", get(file_ep))
        .route(
            "/api/projects/:pid/members",
            get(list_members_ep).post(add_member_ep),
        )
        .route(
            "/api/projects/:pid/members/:username",
            axum::routing::delete(remove_member_ep),
        )
        .route("/api/engines", get(engines_ep))
        .route("/api/analyze-goal", post(analyze_goal_ep))
        .route("/api/projects", get(list_projects).post(create_project))
        .route(
            "/api/projects/:pid",
            axum::routing::delete(delete_project_ep),
        )
        .route("/api/projects/:pid/state", get(state_ep))
        .route("/api/projects/:pid/metrics", get(metrics_ep))
        .route("/api/projects/:pid/runner", get(runner_ep))
        .route("/api/projects/:pid/audit", get(audit_ep))
        .route("/api/projects/:pid/config", get(get_config).put(put_config))
        .route("/api/projects/:pid/control/:action", post(control_ep))
        .route("/api/projects/:pid/ba-analyze", post(ba_analyze))
        .route("/api/projects/:pid/discuss", post(run_discussion_ep))
        .route("/api/projects/:pid/standup", post(standup_ep))
        .route("/api/projects/:pid/tickets", post(create_ticket))
        .route("/api/projects/:pid/ticket/:id", get(ticket_detail_ep))
        .route("/api/projects/:pid/ticket/:id/priority", post(set_priority))
        .route("/api/projects/:pid/ticket/:id/reject", post(reject_ticket))
        .route(
            "/api/projects/:pid/comments",
            get(list_comments).post(post_comment),
        )
        .route(
            "/api/projects/:pid/chat",
            get(chat_list_ep).post(chat_post_ep),
        )
        .route("/api/projects/:pid/chat/ws", get(chat_ws_ep))
        .route(
            "/api/projects/:pid/context",
            get(context_ep).post(context_update_ep),
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

async fn index() -> impl IntoResponse {
    // Always revalidate so a rebuilt dashboard is picked up on reload (the SPA is
    // small; no-cache avoids stale UI after an upgrade).
    (
        [(header::CACHE_CONTROL, "no-cache, must-revalidate")],
        Html(INDEX_HTML),
    )
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
    /// Adopt an existing codebase at this path (brownfield import).
    #[serde(default)]
    existing: Option<String>,
    /// Confirmed project goal/context to seed (from AI-assisted drafting).
    #[serde(default)]
    goal: Option<String>,
}

/// Onboard a new project from the dashboard (greenfield, or brownfield import
/// with `existing`, optionally seeded with a `goal`) via the injected factory.
async fn create_project(
    State(app): State<AppState>,
    Json(req): Json<CreateProjectReq>,
) -> axum::response::Response {
    let Some(factory) = app.factory.clone() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "onboarding is only available in hub mode",
        )
            .into_response();
    };
    let name = req.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    let handle = match factory(NewProjectReq {
        name,
        alias: req.alias,
        existing: req
            .existing
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from),
        goal: req.goal.filter(|s| !s.trim().is_empty()),
    })
    .await
    {
        Ok(h) => h,
        Err(e) => return internal_error(&e),
    };
    let id = handle.id.clone();
    {
        let mut map = app.projects.write().await;
        if map.contains_key(&id) {
            return (StatusCode::CONFLICT, "project id already exists").into_response();
        }
        map.insert(id.clone(), handle);
        app.order.write().await.push(id.clone());
    }
    Json(serde_json::json!({ "ok": true, "id": id })).into_response()
}

/// Delete (deregister) a project: stop its runner, remove it from the hub, and
/// deregister it from the registry. The workspace files are left on disk.
async fn delete_project_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    p.runner.stop();
    app.projects.write().await.remove(&pid);
    app.order.write().await.retain(|id| id != &pid);
    if let Some(remover) = &app.remover {
        if let Err(e) = remover(pid.clone()).await {
            return internal_error(&e);
        }
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[derive(serde::Deserialize)]
struct GoalReq {
    goal: String,
}

/// Refine a rough project goal into a project brief (goal, stack, scope,
/// constraints) via the hub engine — for review before creating the project.
async fn analyze_goal_ep(
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
    };
    match engine.run(request).await {
        Ok(o) if o.succeeded() => {
            Json(serde_json::json!({ "brief": o.stdout.trim() })).into_response()
        }
        Ok(o) => internal_error(&format!("engine failed: {}", o.stderr.trim())),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Serialize state for list views but drop each ticket's heavy `design` specs —
/// the board/backlog/roadmap only need the summary fields. Full specs load
/// on demand via [`ticket_detail_ep`], keeping the 1 Hz SSE payload small.
fn lite_state_value(state: &coxagent_application::ProjectState) -> serde_json::Value {
    let mut v = serde_json::to_value(state).unwrap_or_default();
    if let Some(tickets) = v
        .get_mut("tickets")
        .and_then(serde_json::Value::as_array_mut)
    {
        for t in tickets {
            if let Some(obj) = t.as_object_mut() {
                obj.remove("design");
            }
        }
    }
    // Team chat is delivered instantly over its WebSocket, but we KEEP it in the
    // 1s SSE snapshot too as a fallback: it reaches clients whose WebSocket
    // didn't connect (e.g. a WKWebView) and keeps two hubs sharing one state
    // file in sync. The client merges both sources and de-duplicates.
    v
}

async fn state_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match p.store.load().await {
        Ok(state) => Json(lite_state_value(&state)).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Full detail for one ticket — including the `design` specs stripped from list
/// payloads — loaded only when the user opens it.
async fn ticket_detail_ep(
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
                Json(serde_json::to_value(t).unwrap_or_default()).into_response()
            }),
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
    // Budget caps apply immediately (shared live cell); everything else needs a
    // restart since the runner captured it at spawn.
    if let Ok(mut caps) = p.budget.lock() {
        caps.lifetime_usd = cfg.workflow.budget_usd;
        caps.daily_usd = cfg.policy.daily_budget_usd;
    }
    match serde_json::to_string_pretty(&cfg) {
        Ok(text) => match std::fs::write(&p.config_path, text) {
            Ok(()) => Json(serde_json::json!({
                "ok": true,
                "note": "budget applied live; other changes apply on restart"
            }))
            .into_response(),
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
    #[serde(default)]
    acceptance_criteria: Vec<String>,
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
        acceptance_criteria: req.acceptance_criteria.clone(),
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

/// List the project's team-chat messages (oldest first).
async fn chat_list_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let chat = p.store.load().await.map(|s| s.chat).unwrap_or_default();
    Json(chat).into_response()
}

#[derive(serde::Deserialize)]
struct PostChatReq {
    body: String,
}

/// Post a team-chat message as the signed-in user. Any authenticated principal
/// may post (see the `/chat` carve-out in [`auth_mw`]); the author is the
/// resolved username, or `"user"` when auth is disabled.
async fn chat_post_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PostChatReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let body = req.body.trim();
    if body.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "empty message").into_response();
    }
    if body.chars().count() > 2000 {
        return (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "message too long",
        )
            .into_response();
    }
    let user = match &app.auth {
        Some(auth) => resolve_principal(auth, &headers)
            .await
            .map_or_else(|| "user".to_owned(), |u| u.username),
        None => "user".to_owned(),
    };
    if deliver_chat(&app, &p, &user, body).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        internal_error("chat save failed")
    }
}

/// The project brief agents are seeded with (`project_context.md`): its `Goal`
/// section plus the full markdown, so the dashboard can surface what the team
/// is actually building toward.
async fn context_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let md = tokio::fs::read_to_string(&p.context_path)
        .await
        .unwrap_or_default();
    Json(serde_json::json!({ "goal": extract_goal(&md), "full": md })).into_response()
}

#[derive(serde::Deserialize)]
struct GoalUpdateReq {
    goal: String,
}

/// Update the `## Goal` section of the project brief. Admin-only (enforced by
/// `auth_mw`). Note: the running loop captured its context at startup, so an
/// edited goal seeds agent work from the next restart onward.
async fn context_update_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<GoalUpdateReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let goal = req.goal.trim();
    if goal.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty goal").into_response();
    }
    if goal.chars().count() > 4000 {
        return (StatusCode::PAYLOAD_TOO_LARGE, "goal too long").into_response();
    }
    let md = tokio::fs::read_to_string(&p.context_path)
        .await
        .unwrap_or_default();
    match tokio::fs::write(&p.context_path, replace_goal(&md, goal)).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Extract the body of the `## Goal` markdown section (empty if absent).
fn extract_goal(md: &str) -> String {
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
fn replace_goal(md: &str, goal: &str) -> String {
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

/// Max characters accepted in a single chat message.
const CHAT_MAX_CHARS: usize = 2000;
/// Sliding-window rate limit for a single WebSocket: at most this many messages
/// per [`CHAT_RATE_WINDOW`].
const CHAT_RATE_MAX: usize = 12;
const CHAT_RATE_WINDOW: Duration = Duration::from_secs(10);

/// Live team-chat WebSocket. Requires an authenticated principal (enforced by
/// `auth_mw` on the upgrade GET, re-resolved here for the username) and — as
/// defense-in-depth against cross-site WebSocket hijacking on top of the
/// `SameSite=Strict` session cookie — a same-origin `Origin` header.
async fn chat_ws_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Re-resolve the principal so the socket is attributed to a real user. When
    // auth is disabled (local mode) everyone is "user".
    let user = match &app.auth {
        Some(auth) => match resolve_principal(auth, &headers).await {
            Some(u) => u.username,
            None => {
                return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
            }
        },
        None => "user".to_owned(),
    };
    let ch = app.chat_channel(&pid).await;
    let mut ws = ws;
    ws = ws.max_message_size(64 * 1024);
    ws.on_upgrade(move |socket| chat_socket(socket, app, p, user, ch))
}

/// Same-origin guard: allow when there is no `Origin` (non-browser client) or
/// when its host matches the request `Host`. Blocks browser sockets opened from
/// a different site even if the session cookie were somehow attached.
fn origin_ok(headers: &axum::http::HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin_host = origin.split("://").nth(1).unwrap_or(origin);
    match headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        Some(host) => origin_host == host,
        None => false,
    }
}

/// Drive one chat WebSocket in a single task: fan broadcast messages out to the
/// client while accepting validated, rate-limited messages from it. Using one
/// `select!` loop (rather than splitting the socket) avoids a `futures-util`
/// dependency — axum's `WebSocket` exposes async `recv`/`send` directly.
async fn chat_socket(
    mut socket: WebSocket,
    app: AppState,
    p: ProjectHandle,
    user: String,
    ch: ChatChannel,
) {
    let mut rx = ch.tx.subscribe();
    let mut recv_times: std::collections::VecDeque<std::time::Instant> =
        std::collections::VecDeque::new();
    loop {
        tokio::select! {
            // Server → client: forward a broadcast message.
            bcast = rx.recv() => {
                match bcast {
                    Ok(json) => {
                        if socket.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    // Lagged (slow client) — skip missed messages, keep going.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            // Client → server: validate, rate-limit, persist + broadcast.
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue, // ignore binary/ping/pong
                };
                // Accept either a raw string or {"body": "..."}.
                let body = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("body").and_then(|b| b.as_str()).map(str::to_owned))
                    .unwrap_or(text);
                let body = body.trim();
                if body.is_empty() || body.chars().count() > CHAT_MAX_CHARS {
                    continue;
                }
                let now = std::time::Instant::now();
                while recv_times.front().is_some_and(|t| now.duration_since(*t) > CHAT_RATE_WINDOW) {
                    recv_times.pop_front();
                }
                if recv_times.len() >= CHAT_RATE_MAX {
                    continue; // silently drop; client is flooding
                }
                recv_times.push_back(now);
                deliver_chat(&app, &p, &user, body).await;
            }
        }
    }
}

/// SM-run standup: posts a deterministic status roundup to the team channel and
/// pulls in each agent's latest contribution. Zero engine cost — derived from
/// state — so it can be triggered freely to see the team "gather".
async fn standup_ep(
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
        format!(
            "Standup — Sprint #{} \u{201c}{}\u{201d}: {}/{} committed shipped, {inflight} in flight, {} blocker(s).",
            sp.number,
            sp.goal,
            done,
            sp.committed.len(),
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

    let closing =
        if blockers.is_empty() {
            "Focus: keep burning the sprint backlog — ship before proposing more.".to_owned()
        } else {
            let show: Vec<&String> = blockers.iter().take(3).collect();
            format!(
            "Focus: clear {} open bug(s) first ({}). DEV-BUG, these take priority over features.",
            blockers.len(),
            show.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        )
        };
    s.post_comment("SM", &closing, None);

    match p.store.save(&s).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct PathQuery {
    #[serde(default)]
    path: String,
}

/// Resolve a user-supplied relative path under `root`, rejecting traversal
/// outside it. Returns the canonicalized path when safe.
fn safe_under(root: &std::path::Path, rel: &str) -> Option<PathBuf> {
    // Reject absolute paths and any `..` component outright.
    let candidate = root.join(rel.trim_start_matches('/'));
    let root_c = root.canonicalize().ok()?;
    let cand_c = candidate.canonicalize().ok()?;
    cand_c.starts_with(&root_c).then_some(cand_c)
}

/// Browse the project codebase: lists the directory at `?path=` (relative to the
/// codebase root, default root). Powers the in-app file browser.
async fn workspace_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PathQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let codebase = p.work_dir.clone();
    let dir = if q.path.is_empty() {
        codebase.clone()
    } else {
        match safe_under(&codebase, &q.path) {
            Some(d) if d.is_dir() => d,
            _ => return (StatusCode::BAD_REQUEST, "bad path").into_response(),
        }
    };
    let mut entries: Vec<serde_json::Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if matches!(
                name.as_str(),
                "target" | ".git" | "node_modules" | ".DS_Store"
            ) {
                continue;
            }
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            let size = e.metadata().map_or(0, |m| m.len());
            entries.push(serde_json::json!({ "name": name, "dir": is_dir, "size": size }));
        }
    }
    entries.sort_by(|a, b| {
        let d = |v: &serde_json::Value| {
            v.get("dir")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        d(b).cmp(&d(a)).then_with(|| {
            a.get("name")
                .and_then(serde_json::Value::as_str)
                .cmp(&b.get("name").and_then(serde_json::Value::as_str))
        })
    });
    Json(serde_json::json!({
        "codebase": codebase.display().to_string(),
        "config": p.config_path.display().to_string(),
        "is_git": codebase.join(".git").exists(),
        "path": q.path,
        "entries": entries,
    }))
    .into_response()
}

/// Return the text content of one file under the codebase (path-guarded, capped
/// so the browser stays responsive). Powers the in-app file viewer.
async fn file_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PathQuery>,
) -> axum::response::Response {
    const MAX: u64 = 512 * 1024;
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(file) = safe_under(&p.work_dir, &q.path).filter(|f| f.is_file()) else {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    };
    if file.metadata().map_or(0, |m| m.len()) > MAX {
        return (StatusCode::PAYLOAD_TOO_LARGE, "file too large to preview").into_response();
    }
    match std::fs::read(&file) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => {
                Json(serde_json::json!({ "path": q.path, "content": text })).into_response()
            }
            Err(_) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, "binary file").into_response(),
        },
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
    headers: axum::http::HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let handle = app.project(&pid).await;
    // Identify the viewer so distinct-user counts (not tab counts) are reported.
    let user = match &app.auth {
        Some(auth) => resolve_principal(auth, &headers)
            .await
            .map_or_else(|| "anonymous".to_owned(), |u| u.username),
        None => "local".to_owned(),
    };
    let guard = ViewerGuard::new(&app.viewers, user);
    let stream = IntervalStream::new(tokio::time::interval(STREAM_INTERVAL)).then(move |_| {
        // `guard` is owned by this closure, so the count drops when the stream ends.
        let count = guard.count();
        let handle = handle.clone();
        async move {
            let payload = match handle {
                Some(p) => serde_json::json!({
                    "state": p.store.load().await.ok().as_ref().map(lite_state_value),
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
        && !path.starts_with("/api/auth/2fa/") // self-service, any signed-in user
        && !path.ends_with("/chat"); // team chat is open to any signed-in user
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

#[derive(serde::Deserialize)]
struct MemberReq {
    username: String,
}

/// List every account with a flag for whether it's assigned to this project, so
/// the Team view can show members and offer the rest for assignment (admin-only).
async fn list_members_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(serde_json::json!([])).into_response();
    };
    let is_admin = resolve_principal(&auth, &headers)
        .await
        .is_some_and(|u| u.role.can_write());
    if !is_admin {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    let out: Vec<serde_json::Value> = auth
        .list_users()
        .await
        .into_iter()
        .map(|u| {
            serde_json::json!({
                "username": u.username,
                "role": u.role,
                "assigned": u.projects.iter().any(|p| p == &pid),
            })
        })
        .collect();
    Json(out).into_response()
}

/// Assign a user to a project (admin-only via the write gate).
async fn add_member_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<MemberReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if auth.assign_project(req.username.trim(), &pid).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::CONFLICT, "unknown user").into_response()
    }
}

/// Remove a user from a project (admin-only via the write gate).
async fn remove_member_ep(
    State(app): State<AppState>,
    Path((pid, username)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if auth.unassign_project(&username, &pid).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::CONFLICT, "not a member").into_response()
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

/// Running per-user aggregate used by [`people_analytics_ep`].
struct PeopleAgg {
    actions: u32,
    work: u32,
    failures: u32,
    last_active: String,
    days: std::collections::BTreeSet<String>,
    by_action: std::collections::BTreeMap<String, u32>,
}

/// Per-user activity analytics for admins: who is actually working, how much,
/// how recently, and how effectively. Derived from the audit trail so it needs
/// no extra storage. Newest audit window (up to 5000 rows) is aggregated per
/// user into totals, work vs. sign-in actions, success rate, active days, and a
/// simple productivity score.
async fn people_analytics_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Some(auth) = app.auth.clone() {
        let ok = match resolve_principal(&auth, &headers).await {
            Some(u) => u.role.can_write(),
            None => false,
        };
        if !ok {
            return (StatusCode::FORBIDDEN, "admin role required").into_response();
        }
    }
    let entries = match app.audit.recent(5000).await {
        Ok(e) => e,
        Err(e) => return internal_error(&e.to_string()),
    };

    let mut per: HashMap<String, PeopleAgg> = HashMap::new();
    for e in &entries {
        let a = per.entry(e.user.clone()).or_insert_with(|| PeopleAgg {
            actions: 0,
            work: 0,
            failures: 0,
            last_active: String::new(),
            days: std::collections::BTreeSet::new(),
            by_action: std::collections::BTreeMap::new(),
        });
        a.actions += 1;
        // "Work" = anything that changes state, i.e. not a passive sign-in/read.
        let sign_in = matches!(e.action.as_str(), "login" | "logout" | "2fa-verify");
        if !sign_in {
            a.work += 1;
        }
        if e.status >= 400 {
            a.failures += 1;
        }
        if e.at > a.last_active {
            a.last_active.clone_from(&e.at);
        }
        if let Some(day) = e.at.split('T').next() {
            a.days.insert(day.to_owned());
        }
        *a.by_action.entry(e.action.clone()).or_insert(0) += 1;
    }

    let mut people: Vec<serde_json::Value> = per
        .into_iter()
        .map(|(user, a)| {
            let success = if a.actions == 0 {
                100.0
            } else {
                f64::from(a.actions - a.failures) / f64::from(a.actions) * 100.0
            };
            // Top actions, most frequent first.
            let mut top: Vec<(String, u32)> = a.by_action.into_iter().collect();
            top.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
            top.truncate(4);
            serde_json::json!({
                "user": user,
                "actions": a.actions,
                "work": a.work,
                "success_rate": (success * 10.0).round() / 10.0,
                "active_days": a.days.len(),
                "last_active": a.last_active,
                "top_actions": top.iter().map(|(k,v)| serde_json::json!({"action":k,"count":v})).collect::<Vec<_>>(),
            })
        })
        .collect();
    // Busiest workers first.
    people.sort_by_key(|p| {
        std::cmp::Reverse(
            p.get("work")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        )
    });
    Json(serde_json::json!({ "people": people, "sample": entries.len() })).into_response()
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
/// Turn a User-Agent string into a friendly "Browser on OS" device label.
fn device_label(ua: &str) -> String {
    if ua.trim().is_empty() {
        return "Unknown device".to_owned();
    }
    let browser = if ua.contains("Edg") {
        "Edge"
    } else if ua.contains("OPR") || ua.contains("Opera") {
        "Opera"
    } else if ua.contains("Chrome") {
        "Chrome"
    } else if ua.contains("Firefox") {
        "Firefox"
    } else if ua.contains("Safari") {
        "Safari"
    } else {
        "Browser"
    };
    let os = if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("iPhone") {
        "iPhone"
    } else if ua.contains("iPad") {
        "iPad"
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        "device"
    };
    format!("{browser} on {os}")
}

/// The signed-in user's active sessions (where they're logged in), for the
/// "your devices" view. The caller's own session is flagged `current`.
async fn sessions_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(serde_json::json!([])).into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
    };
    let token = cookie_value(&headers, SESSION_COOKIE).unwrap_or_default();
    Json(auth.sessions_for(&user.username, &token).await).into_response()
}

async fn login_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
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
        LoginResult::Ok(token) => {
            // Label the session with the device/browser from the User-Agent.
            let ua = headers
                .get(axum::http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            auth.attach_device(&token, &device_label(ua)).await;
            token
        }
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
