//! HTTP server (inbound adapter) — a multi-project hub. Serves the embedded
//! dashboard plus a per-project JSON API, SSE stream, controllable runner, and
//! ticket actions. A single-project `serve` registers one project; `hub`
//! registers many. Routes are scoped `/api/projects/:pid/...`.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{delete, get, post};
use axum::Router;
use coxagent_application::auth::AuthPort;
use coxagent_application::metrics;
use coxagent_application::ports::outbound::{AuditPort, AuditRecord, StateStorePort};
use coxagent_application::state::ChatMsg;
use coxagent_application::use_cases::RunnerHandle;
use coxagent_application::Config;
use coxagent_application::DocPage;
use serde_json::json;
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
// Vendored terminal assets — embedded so the terminal works offline/air-gapped.
const XTERM_JS: &str = include_str!("web/xterm.min.js");
const XTERM_CSS: &str = include_str!("web/xterm.min.css");
const XTERM_FIT_JS: &str = include_str!("web/xterm-addon-fit.min.js");

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
    /// The code host for review actions (list/merge/diff PRs); set when git
    /// integration is configured with a provider.
    pub forge: Option<Arc<dyn coxagent_application::ports::outbound::ForgePort>>,
    /// Deploy adapter, so on-demand actions (e.g. a chat "deploy" request) can
    /// build & run the app.
    pub deploy: Option<Arc<dyn coxagent_application::ports::outbound::DeployPort>>,
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
    /// Import straight from a git URL: the factory clones it into the
    /// project workspace, then adopts it like any existing codebase (remote
    /// auto-detected, config pre-filled).
    pub git_url: Option<String>,
    pub goal: Option<String>,
}

/// Deregisters a project (removes it from the hub registry), injected by the
/// composition root. Returns an error message on failure.
pub type ProjectRemover =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

/// Record one audit entry through the injected sink (fire-and-forget).
/// Baseline security headers on every response: no MIME sniffing, no framing
/// (clickjacking), same-origin referrers.
async fn security_headers_mw(
    req: Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "X-Content-Type-Options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        "X-Frame-Options",
        axum::http::HeaderValue::from_static("DENY"),
    );
    h.insert(
        "Referrer-Policy",
        axum::http::HeaderValue::from_static("same-origin"),
    );
    resp
}

/// Extract the project ID from a URL path like `/api/projects/:pid/...`.
fn extract_pid_from_path(path: &str) -> Option<&str> {
    // match /api/projects/<pid> or /api/projects/<pid>/...
    if let Some(rest) = path.strip_prefix("/api/projects/") {
        rest.split('/').next()
    } else {
        None
    }
}

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

/// Hub-level, system-wide chat: one store shared across every project. Holds the
/// [`SystemChat`] aggregate (private channels + all messages) behind a mutex,
/// the JSON file it persists to, a broadcast bus for live WebSockets, and the
/// directory uploaded chat media lives in.
/// Hub-wide chat key in the shared KV store.
const SYSCHAT_KEY: &str = "system_chat";

#[derive(Clone)]
struct SysChat {
    inner: Arc<tokio::sync::Mutex<coxagent_application::SystemChat>>,
    /// Local-file fallback path, used only when no shared store is configured.
    path: PathBuf,
    /// Shared DB store (Postgres). When set, it is the system of record and the
    /// file is not touched.
    store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    tx: tokio::sync::broadcast::Sender<String>,
}

impl SysChat {
    /// Load the store from the shared DB when `store` is set, else from
    /// `dir/system_chat.json` (empty if absent).
    async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("system_chat.json");
        let text = if let Some(s) = &store {
            s.load(SYSCHAT_KEY).await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
            tx: tokio::sync::broadcast::channel(256).0,
        }
    }

    /// Persist the current state (best-effort) to the shared DB, or the local
    /// file when no store is configured.
    async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save(SYSCHAT_KEY, &json).await {
                tracing::warn!("system chat save failed: {e}");
            }
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, json);
    }
}

/// Workspace identity + invites: the company-level document (name, branding,
/// pending invite links) persisted in the shared KV store (Postgres) when
/// configured, else a local `workspace.json` under the hub dir.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct WorkspaceDoc {
    #[serde(default)]
    name: String,
    #[serde(default)]
    tagline: String,
    #[serde(default)]
    accent: String,
    /// Company-wide engineering conventions (coding standards, style, do/don't).
    /// Injected into every agent's prompt across every project.
    #[serde(default)]
    conventions: String,
    #[serde(default)]
    invites: Vec<Invite>,
    /// Client-app distribution: where users download CoXAgent for each
    /// platform, refreshed automatically from GitHub Releases when
    /// `releases_repo` is set (manual URLs act as overrides).
    #[serde(default)]
    downloads: DownloadsCfg,
}

/// Per-platform download links + the release source of truth.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct DownloadsCfg {
    /// `owner/name` GitHub repo whose Releases carry the app builds. When set,
    /// a background task polls the latest release and fills version + asset
    /// URLs automatically after every deploy that tags a release.
    #[serde(default)]
    releases_repo: String,
    /// Newest published app version (auto from releases, or set manually).
    #[serde(default)]
    latest_version: String,
    #[serde(default)]
    macos: String,
    #[serde(default)]
    windows: String,
    #[serde(default)]
    linux: String,
    /// App Store / TestFlight link — iOS can't sideload, so this is a URL only.
    #[serde(default)]
    ios: String,
    /// Release notes of the latest version (from the GitHub release body,
    /// capped) — shown as "What's new" in the update modal.
    #[serde(default)]
    notes: String,
}

/// One shareable invite link: whoever opens it can create their own account
/// with the preset role + project membership, `uses_left` times.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Invite {
    token: String,
    role: String,
    #[serde(default)]
    projects: Vec<String>,
    created_by: String,
    created_at: String,
    uses_left: u32,
}

/// One space: an organizational unit grouping projects + members under its own
/// admins. Spaces live in the shared KV (`app_kv` key `spaces`); a normal admin
/// manages only spaces that list them, a super admin manages all.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Space {
    /// URL-safe slug id.
    id: String,
    name: String,
    #[serde(default)]
    tagline: String,
    /// Usernames who administer THIS space (invite, edit, assign projects).
    #[serde(default)]
    admins: Vec<String>,
    /// Project ids belonging to this space.
    #[serde(default)]
    projects: Vec<String>,
    /// Explicit member usernames. Saving the space additionally ASSIGNS each
    /// member to every project of the space (additive — never auto-revokes).
    #[serde(default)]
    members: Vec<String>,
    /// Monthly USD spend cap for this space; 0 = no cap. Set by Super only.
    #[serde(default)]
    budget_usd: f64,
    #[serde(default)]
    created_by: String,
    #[serde(default)]
    created_at: String,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct SpacesDoc {
    #[serde(default)]
    spaces: Vec<Space>,
}

/// The hub-wide spaces store (see [`SpacesDoc`]).
#[derive(Clone)]
struct Sp {
    inner: Arc<tokio::sync::Mutex<SpacesDoc>>,
    path: PathBuf,
    store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

impl Sp {
    async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("spaces.json");
        let text = if let Some(s) = &store {
            s.load("spaces").await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
        }
    }

    async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save("spaces", &json).await {
                tracing::warn!("spaces save failed: {e}");
            }
            return;
        }
        let _ = std::fs::write(&self.path, json);
    }
}

/// The hub-wide workspace store (see [`WorkspaceDoc`]).
#[derive(Clone)]
struct Ws {
    inner: Arc<tokio::sync::Mutex<WorkspaceDoc>>,
    path: PathBuf,
    store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

impl Ws {
    async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("workspace.json");
        let text = if let Some(s) = &store {
            s.load("workspace").await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
        }
    }

    async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save("workspace", &json).await {
                tracing::warn!("workspace save failed: {e}");
            }
            return;
        }
        let _ = std::fs::write(&self.path, json);
    }
}

/// A booked meeting. Times are RFC3339 UTC; the watchdog drives reminders,
/// start announcements, and auto-ringing of absent participants.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct Meeting {
    id: String,
    title: String,
    /// RFC3339 start instant.
    start: String,
    duration_min: u32,
    created_by: String,
    participants: Vec<String>,
    /// Minutes before start to remind (0 = no reminder).
    #[serde(default)]
    remind_min: u32,
    /// Who has actually entered the meeting room.
    #[serde(default)]
    joined: Vec<String>,
    #[serde(default)]
    reminded: bool,
    #[serde(default)]
    start_announced: bool,
    /// One automatic ring of the not-yet-joined, ~1 min after start.
    #[serde(default)]
    auto_rang: bool,
    #[serde(default)]
    cancelled: bool,
    #[serde(default)]
    agenda: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct MeetingsDoc {
    meetings: Vec<Meeting>,
}

/// Meeting store: shared KV (`app_kv` key `meetings`) when configured, else a
/// local `meetings.json` under the hub dir — same shape as [`Ws`].
#[derive(Clone)]
struct Mt {
    inner: Arc<tokio::sync::Mutex<MeetingsDoc>>,
    path: PathBuf,
    store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

impl Mt {
    async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("meetings.json");
        let text = if let Some(s) = &store {
            s.load("meetings").await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
        }
    }

    async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save("meetings", &json).await {
                tracing::warn!("meetings save failed: {e}");
            }
            return;
        }
        let _ = std::fs::write(&self.path, json);
    }
}

/// A user's public profile bits: avatar + Slack-style status (emoji + text).
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct Profile {
    #[serde(default)]
    avatar: String,
    #[serde(default)]
    status_emoji: String,
    #[serde(default)]
    status_text: String,
    #[serde(default)]
    at: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ProfilesDoc {
    profiles: std::collections::HashMap<String, Profile>,
}

/// Profile store: shared KV (`app_kv` key `profiles`) when configured, else a
/// local `profiles.json` under the hub dir — same shape as [`Ws`]/[`Mt`].
#[derive(Clone)]
struct Pf {
    inner: Arc<tokio::sync::Mutex<ProfilesDoc>>,
    path: PathBuf,
    store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

impl Pf {
    async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("profiles.json");
        let text = if let Some(s) = &store {
            s.load("profiles").await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
        }
    }

    async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save("profiles", &json).await {
                tracing::warn!("profiles save failed: {e}");
            }
            return;
        }
        let _ = std::fs::write(&self.path, json);
    }
}

/// All profiles — any signed-in user (needed to render avatars/status).
async fn profiles_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if principal_name(&app, &headers).await.is_none() {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    }
    let doc = app.profiles.inner.lock().await;
    Json(&doc.profiles).into_response()
}

#[derive(serde::Deserialize)]
struct ProfileReq {
    #[serde(default)]
    status_emoji: String,
    #[serde(default)]
    status_text: String,
}

/// Update the caller's OWN status (emoji + text). Empty strings clear it.
async fn profile_set_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ProfileReq>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let emoji: String = req.status_emoji.chars().take(8).collect();
    let text: String = req.status_text.trim().chars().take(80).collect();
    {
        let mut doc = app.profiles.inner.lock().await;
        let p = doc.profiles.entry(user.clone()).or_default();
        p.status_emoji = emoji;
        p.status_text = text;
        p.at = now_rfc3339();
    }
    app.profiles.save().await;
    audit_push(&app.audit, &user, "profile status updated".to_owned(), 200).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Clear the caller's OWN avatar, falling back to the initials tile. Upload
/// without a way back out leaves a bad photo stuck forever.
async fn profile_avatar_clear_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    {
        let mut doc = app.profiles.inner.lock().await;
        let p = doc.profiles.entry(user.clone()).or_default();
        p.avatar.clear();
        p.at = now_rfc3339();
    }
    app.profiles.save().await;
    audit_push(&app.audit, &user, "avatar removed".to_owned(), 200).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Upload the caller's OWN avatar (image, ≤ 2 MB). Served via chat media.
async fn profile_avatar_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    mut multipart: axum::extract::Multipart,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let Ok(Some(field)) = multipart.next_field().await else {
        return (StatusCode::BAD_REQUEST, "no file").into_response();
    };
    let mime = field.content_type().unwrap_or("").to_owned();
    if !mime.starts_with("image/") {
        return (StatusCode::BAD_REQUEST, "avatar must be an image").into_response();
    }
    let data = match field.bytes().await {
        Ok(b) if b.len() <= 2 * 1024 * 1024 => b,
        Ok(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "max 2MB").into_response(),
        Err(_) => return (StatusCode::BAD_REQUEST, "read failed").into_response(),
    };
    let ext = mime.strip_prefix("image/").unwrap_or("png");
    let stored = format!("{}-avatar.{}", mint_media_token(), sanitize_name(ext));
    if app
        .storage
        .put(&format!("chat/{stored}"), &data, &mime)
        .await
        .is_err()
    {
        return internal_error("write failed");
    }
    let url = format!("/api/chat/media/{stored}");
    {
        let mut doc = app.profiles.inner.lock().await;
        let p = doc.profiles.entry(user.clone()).or_default();
        p.avatar.clone_from(&url);
        p.at = now_rfc3339();
    }
    app.profiles.save().await;
    audit_push(&app.audit, &user, "avatar updated".to_owned(), 200).await;
    Json(serde_json::json!({ "ok": true, "url": url })).into_response()
}

/// One meeting-event frame, delivered over the system-chat WebSocket to a
/// single user (`to`-filtered by the socket loop, like call signaling).
fn meeting_frame(kind: &str, to: &str, m: &Meeting) -> String {
    serde_json::json!({
        "type": "signal", "from": "SYSTEM", "to": to, "kind": kind,
        "payload": { "meeting": {
            "id": m.id, "title": m.title, "start": m.start,
            "duration_min": m.duration_min, "created_by": m.created_by,
            "participants": m.participants, "joined": m.joined,
        }}
    })
    .to_string()
}

fn parse_rfc3339(s: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

/// Drives the meeting lifecycle: reminder before start, a "meeting started"
/// nudge to everyone at start, and ONE automatic ring of participants who
/// still haven't joined a minute in. Meetings a day past their end are pruned.
async fn meeting_watchdog(app: AppState) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        let now = time::OffsetDateTime::now_utc();
        let mut frames: Vec<String> = Vec::new();
        let mut dirty = false;
        {
            let mut doc = app.meetings.inner.lock().await;
            doc.meetings.retain(|m| {
                let keep = parse_rfc3339(&m.start).is_none_or(|s| {
                    now < s
                        + time::Duration::minutes(i64::from(m.duration_min))
                        + time::Duration::days(1)
                });
                if !keep {
                    dirty = true;
                }
                keep
            });
            for m in &mut doc.meetings {
                if m.cancelled {
                    continue;
                }
                let Some(start) = parse_rfc3339(&m.start) else {
                    continue;
                };
                let end = start + time::Duration::minutes(i64::from(m.duration_min));
                if m.remind_min > 0
                    && !m.reminded
                    && now >= start - time::Duration::minutes(i64::from(m.remind_min))
                    && now < start
                {
                    m.reminded = true;
                    dirty = true;
                    for u in &m.participants {
                        frames.push(meeting_frame("meeting-remind", u, m));
                    }
                }
                if !m.start_announced && now >= start && now < end {
                    m.start_announced = true;
                    dirty = true;
                    for u in &m.participants {
                        frames.push(meeting_frame("meeting-start", u, m));
                    }
                }
                if !m.auto_rang && now >= start + time::Duration::seconds(60) && now < end {
                    m.auto_rang = true;
                    dirty = true;
                    for u in &m.participants {
                        if !m.joined.contains(u) {
                            frames.push(meeting_frame("meeting-ring", u, m));
                        }
                    }
                }
            }
        }
        if dirty {
            app.meetings.save().await;
        }
        for f in frames {
            let _ = app.syschat.tx.send(f);
        }
    }
}

#[derive(serde::Deserialize)]
struct MeetingReq {
    title: String,
    start: String,
    duration_min: Option<u32>,
    participants: Vec<String>,
    #[serde(default)]
    remind_min: Option<u32>,
    #[serde(default)]
    agenda: Option<String>,
}

/// List meetings the caller is part of (participant or creator), soonest first.
async fn meetings_list_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let doc = app.meetings.inner.lock().await;
    let mut mine: Vec<Meeting> = doc
        .meetings
        .iter()
        .filter(|m| !m.cancelled && (m.created_by == user || m.participants.contains(&user)))
        .cloned()
        .collect();
    mine.sort_by(|a, b| a.start.cmp(&b.start));
    Json(mine).into_response()
}

/// Book a meeting. Any signed-in user; the creator is always a participant.
/// Every invitee gets an immediate in-app invite frame.
async fn meeting_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<MeetingReq>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let title = req.title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return (StatusCode::BAD_REQUEST, "title is required (≤120 chars)").into_response();
    }
    let Some(start) = parse_rfc3339(&req.start) else {
        return (StatusCode::BAD_REQUEST, "start must be RFC3339").into_response();
    };
    if start < time::OffsetDateTime::now_utc() - time::Duration::minutes(1) {
        return (StatusCode::BAD_REQUEST, "start is in the past").into_response();
    }
    let mut participants: Vec<String> = req
        .participants
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    if !participants.iter().any(|p| p == &user) {
        participants.push(user.clone());
    }
    participants.dedup();
    let m = Meeting {
        id: format!("mtg-{:08x}", rand_u32()),
        title: title.to_owned(),
        start: req.start.clone(),
        duration_min: req.duration_min.unwrap_or(30).clamp(5, 480),
        created_by: user.clone(),
        participants,
        remind_min: req.remind_min.unwrap_or(10).min(1440),
        agenda: req.agenda.unwrap_or_default(),
        ..Meeting::default()
    };
    {
        let mut doc = app.meetings.inner.lock().await;
        doc.meetings.push(m.clone());
    }
    app.meetings.save().await;
    audit_push(
        &app.audit,
        &user,
        format!("meeting booked: {} ({})", m.title, m.id),
        200,
    )
    .await;
    for u in m.participants.iter().filter(|u| **u != user) {
        let _ = app.syschat.tx.send(meeting_frame("meeting-invite", u, &m));
    }
    Json(m).into_response()
}

#[derive(serde::Deserialize)]
struct MeetingPatch {
    #[serde(default)]
    cancel: Option<bool>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    duration_min: Option<u32>,
    #[serde(default)]
    participants: Option<Vec<String>>,
    #[serde(default)]
    agenda: Option<String>,
}

/// Edit or cancel a meeting — creator or a management role only.
async fn meeting_patch_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<MeetingPatch>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(caller) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let mut notify: Vec<String> = Vec::new();
    let mut cancelled_m: Option<Meeting> = None;
    {
        let mut doc = app.meetings.inner.lock().await;
        let Some(m) = doc.meetings.iter_mut().find(|m| m.id == id) else {
            return (StatusCode::NOT_FOUND, "no such meeting").into_response();
        };
        if m.created_by != caller.username && !caller.role.can_manage() {
            return (StatusCode::FORBIDDEN, "only the organiser can change this").into_response();
        }
        if req.cancel == Some(true) {
            m.cancelled = true;
            cancelled_m = Some(m.clone());
        } else {
            if let Some(t) = &req.title {
                if !t.trim().is_empty() {
                    m.title = t.trim().to_owned();
                }
            }
            if let Some(s) = &req.start {
                if parse_rfc3339(s).is_some() {
                    m.start = s.clone();
                    // A moved meeting reminds/announces again at the new time.
                    m.reminded = false;
                    m.start_announced = false;
                    m.auto_rang = false;
                }
            }
            if let Some(d) = req.duration_min {
                m.duration_min = d.clamp(5, 480);
            }
            if let Some(p) = req.participants {
                let mut p: Vec<String> = p
                    .into_iter()
                    .map(|x| x.trim().to_owned())
                    .filter(|x| !x.is_empty())
                    .collect();
                if !p.iter().any(|x| x == &m.created_by) {
                    p.push(m.created_by.clone());
                }
                p.dedup();
                m.participants = p;
            }
            if let Some(a) = req.agenda {
                m.agenda = a;
            }
            notify.clone_from(&m.participants);
        }
    }
    app.meetings.save().await;
    if let Some(m) = &cancelled_m {
        for u in &m.participants {
            let _ = app.syschat.tx.send(meeting_frame("meeting-cancel", u, m));
        }
    }
    audit_push(
        &app.audit,
        &caller.username,
        format!("meeting updated: {id}"),
        200,
    )
    .await;
    let _ = notify;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Enter the meeting room: records the caller as joined and returns the
/// meeting (with who's already in) so the client can offer to present peers.
async fn meeting_join_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let out;
    {
        let mut doc = app.meetings.inner.lock().await;
        let Some(m) = doc.meetings.iter_mut().find(|m| m.id == id) else {
            return (StatusCode::NOT_FOUND, "no such meeting").into_response();
        };
        if !m.participants.contains(&user) && m.created_by != user {
            return (StatusCode::FORBIDDEN, "not invited").into_response();
        }
        if !m.joined.contains(&user) {
            m.joined.push(user.clone());
        }
        out = m.clone();
    }
    app.meetings.save().await;
    Json(out).into_response()
}

#[derive(serde::Deserialize)]
struct MeetingRingReq {
    user: String,
}

/// Ring one participant who hasn't joined — any participant can nudge.
async fn meeting_ring_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<MeetingRingReq>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let m = {
        let doc = app.meetings.inner.lock().await;
        let Some(m) = doc.meetings.iter().find(|m| m.id == id) else {
            return (StatusCode::NOT_FOUND, "no such meeting").into_response();
        };
        if !m.participants.contains(&user) && m.created_by != user {
            return (StatusCode::FORBIDDEN, "not invited").into_response();
        }
        if !m.participants.contains(&req.user) {
            return (StatusCode::BAD_REQUEST, "target is not a participant").into_response();
        }
        m.clone()
    };
    let _ = app
        .syschat
        .tx
        .send(meeting_frame("meeting-ring", &req.user, &m));
    audit_push(
        &app.audit,
        &user,
        format!("meeting ring: {} → {}", id, req.user),
        200,
    )
    .await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// The caller's username, via session cookie or bearer token.
async fn principal_name(app: &AppState, headers: &axum::http::HeaderMap) -> Option<String> {
    let auth = app.auth.clone()?;
    resolve_principal(&auth, headers).await.map(|u| u.username)
}

fn rand_u32() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish() as u32
}

/// Default blob storage: local disk under a root, used when no S3/MinIO backend
/// is injected. Keys are relative paths (e.g. `chat/<file>`).
struct DiskStorage {
    root: PathBuf,
}

#[async_trait::async_trait]
impl coxagent_application::ports::outbound::StoragePort for DiskStorage {
    async fn put(
        &self,
        key: &str,
        data: &[u8],
        _mime: &str,
    ) -> Result<(), coxagent_application::PortError> {
        if key.contains("..") {
            return Err(coxagent_application::PortError::Backend(
                "bad key".to_owned(),
            ));
        }
        let path = self.root.join(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| coxagent_application::PortError::Backend(e.to_string()))?;
        }
        std::fs::write(&path, data)
            .map_err(|e| coxagent_application::PortError::Backend(e.to_string()))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, coxagent_application::PortError> {
        if key.contains("..") {
            return Err(coxagent_application::PortError::Backend(
                "bad key".to_owned(),
            ));
        }
        std::fs::read(self.root.join(key))
            .map_err(|e| coxagent_application::PortError::Backend(e.to_string()))
    }
}

#[derive(Clone)]
struct AppState {
    projects: Arc<RwLock<HashMap<String, ProjectHandle>>>,
    /// Per-project team-chat channels, created lazily on first use.
    chat_bus: Arc<RwLock<HashMap<String, ChatChannel>>>,
    /// Per-document collaboration channels (live edit), keyed `"<pid>/<docid>"`.
    docs_bus: Arc<RwLock<HashMap<String, tokio::sync::broadcast::Sender<String>>>>,
    /// Live editors per document room: room → (username → open-connection count).
    docs_editors: Arc<std::sync::Mutex<HashMap<String, HashMap<String, usize>>>>,
    order: Arc<RwLock<Vec<String>>>,
    factory: Option<ProjectFactory>,
    auth: Option<Arc<dyn AuthPort>>,
    audit: Arc<dyn AuditPort>,
    /// Agent CLIs detected on this machine's PATH: `(name, path)`.
    engines: Arc<Vec<(String, String)>>,
    /// Developer tooling status (git/gh/glab/docker), computed at startup.
    tooling: Arc<serde_json::Value>,
    /// Live viewers keyed by username → open-connection count. Distinct users =
    /// map length, so one person in the app + a browser tab counts once.
    viewers: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    /// Deregisters a project from the hub registry.
    remover: Option<ProjectRemover>,
    /// System-wide chat store shared across all projects.
    syschat: SysChat,
    /// Workspace identity + invite links (company-level, hub-wide).
    workspace: Ws,
    /// Multi-space registry (super-admin managed groups of projects+admins).
    spaces: Sp,
    /// Booked meetings (calendar) — reminders/rings driven by a watchdog.
    meetings: Mt,
    /// User avatars + Slack-style statuses (self-service).
    profiles: Pf,
    /// Blob storage for uploaded files (local disk by default, or S3/MinIO).
    storage: Arc<dyn coxagent_application::ports::outbound::StoragePort>,
    /// Server-side documentation store (MongoDB) when configured; `None` falls
    /// back to per-project `state.json`.
    doc_store: Option<Arc<dyn coxagent_application::ports::outbound::DocStorePort>>,
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

    /// The usernames currently connected (for per-channel online presence).
    fn online_users(&self) -> Vec<String> {
        self.viewers
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
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

    // --- Documentation persistence -------------------------------------------
    // These route through the MongoDB doc store when configured, else the
    // project's `state.json`. Callers never branch on the backend themselves.

    /// All documentation pages for a project.
    async fn doc_list(&self, pid: &str, p: &ProjectHandle) -> Vec<DocPage> {
        // The DOCS agent writes pages into project state; a Mongo store (when
        // configured) holds user-authored/collab pages. Merge both so the Wiki
        // shows everything, agent pages included, deduped by id.
        let mut pages = p.store.load().await.map(|s| s.docs).unwrap_or_default();
        if let Some(ds) = &self.doc_store {
            for page in ds.list(pid).await.unwrap_or_default() {
                if let Some(existing) = pages.iter_mut().find(|d| d.id == page.id) {
                    *existing = page;
                } else {
                    pages.push(page);
                }
            }
        }
        pages
    }

    /// One documentation page by id.
    async fn doc_get(&self, pid: &str, p: &ProjectHandle, id: &str) -> Option<DocPage> {
        if let Some(ds) = &self.doc_store {
            if let Ok(Some(page)) = ds.get(pid, id).await {
                return Some(page);
            }
        }
        p.store.load().await.ok().and_then(|s| s.doc(id))
    }

    /// Create or replace a page; returns the stored page (id minted if empty).
    #[allow(clippy::too_many_arguments)]
    async fn doc_upsert(
        &self,
        pid: &str,
        p: &ProjectHandle,
        id: &str,
        folder: &str,
        title: &str,
        body: &str,
        author: &str,
    ) -> Result<DocPage, String> {
        let category = doc_category(folder).to_owned();
        if let Some(ds) = &self.doc_store {
            let page = DocPage {
                id: if id.is_empty() {
                    mint_doc_id()
                } else {
                    id.to_owned()
                },
                folder: folder.to_owned(),
                category,
                title: title.to_owned(),
                body: body.to_owned(),
                updated_at: coxagent_application::state::now_rfc3339(),
                updated_by: author.to_owned(),
            };
            ds.upsert(pid, &page).await.map_err(|e| e.to_string())?;
            return Ok(page);
        }
        let mut state = p.store.load().await.map_err(|e| e.to_string())?;
        let page = state.upsert_doc(id, folder, &category, title, body, author);
        p.store.save(&state).await.map_err(|e| e.to_string())?;
        Ok(page)
    }

    /// Delete a page by id; returns whether one existed.
    async fn doc_delete(&self, pid: &str, p: &ProjectHandle, id: &str) -> Result<bool, String> {
        if let Some(ds) = &self.doc_store {
            return ds.delete(pid, id).await.map_err(|e| e.to_string());
        }
        let mut state = p.store.load().await.map_err(|e| e.to_string())?;
        let removed = state.remove_doc(id);
        p.store.save(&state).await.map_err(|e| e.to_string())?;
        Ok(removed)
    }

    /// Get (or lazily create) the live-edit broadcast channel for a doc room.
    async fn docs_room(&self, room: &str) -> tokio::sync::broadcast::Sender<String> {
        if let Some(tx) = self.docs_bus.read().await.get(room) {
            return tx.clone();
        }
        self.docs_bus
            .write()
            .await
            .entry(room.to_owned())
            .or_insert_with(|| tokio::sync::broadcast::channel(64).0)
            .clone()
    }

    /// Register/deregister a live editor in a room; returns the current roster.
    fn docs_presence(&self, room: &str, user: &str, joined: bool) -> Vec<String> {
        let mut map = match self.docs_editors.lock() {
            Ok(m) => m,
            Err(p) => p.into_inner(),
        };
        let room_map = map.entry(room.to_owned()).or_default();
        if joined {
            *room_map.entry(user.to_owned()).or_insert(0) += 1;
        } else if let Some(n) = room_map.get_mut(user) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                room_map.remove(user);
            }
        }
        let mut names: Vec<String> = room_map.keys().cloned().collect();
        names.sort();
        if room_map.is_empty() {
            map.remove(room);
        }
        names
    }

    /// Get (or lazily create) the live chat channel for a project.
    async fn chat_channel(&self, pid: &str) -> ChatChannel {
        // Use write lock directly to avoid TOCTOU race between read and write.
        self.chat_bus
            .write()
            .await
            .entry(pid.to_owned())
            .or_insert_with(|| ChatChannel {
                tx: tokio::sync::broadcast::channel(256).0,
                write_lock: Arc::new(tokio::sync::Mutex::new(())),
            })
            .clone()
    }

    /// Build the live [`ChatContext`] — the users and projects the system-chat
    /// model needs to compute `#general`/project-channel membership.
    async fn chat_context(&self) -> coxagent_application::ChatContext {
        use coxagent_application::{ChatContext, ProjectRef, UserRef};
        let projects: Vec<ProjectRef> = {
            let map = self.projects.read().await;
            let order = self.order.read().await;
            order
                .iter()
                .filter_map(|id| map.get(id))
                .map(|h| ProjectRef {
                    id: h.id.clone(),
                    alias: h.alias.clone(),
                    name: h.name.clone(),
                })
                .collect()
        };
        let users: Vec<UserRef> = match &self.auth {
            Some(a) => a
                .list_users()
                .await
                .into_iter()
                .map(|u| UserRef {
                    admin: u.role.as_str() == "admin",
                    username: u.username,
                    projects: u.projects,
                })
                .collect(),
            None => Vec::new(),
        };
        ChatContext { users, projects }
    }
}

/// Persist one chat message and fan it out to every live WebSocket. The
/// per-project `write_lock` serializes the load→append→save so concurrent
/// senders can't lose each other's messages. Returns `false` if persistence
/// fails. `body` must already be validated (non-empty, length-capped).
async fn deliver_chat(
    app: &AppState,
    p: &ProjectHandle,
    user: &str,
    body: &str,
    channel: &str,
    attachments: Vec<coxagent_application::Attachment>,
) -> bool {
    let ch = app.chat_channel(&p.id).await;
    let _guard = ch.write_lock.lock().await;
    let Ok(mut state) = p.store.load().await else {
        return false;
    };
    // Enforce membership: only people who can view a channel may post to it.
    match state.channel(channel) {
        Some(c) if c.can_view(user) => {}
        _ => return false,
    }
    state.post_chat_in(user, body, channel, attachments);
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
    /// Developer tooling status JSON (git/gh/glab/docker); `Null` when unknown.
    pub tooling: serde_json::Value,
    /// A hub-level engine + work dir for cross-project drafting (project goals).
    pub analyzer: Option<(
        Arc<dyn coxagent_application::ports::outbound::AgentEnginePort>,
        PathBuf,
    )>,
    /// Directory holding hub-wide state (system chat + its media). Defaults to
    /// the current directory when unset.
    pub hub_dir: Option<PathBuf>,
    /// Blob storage backend for uploads. Defaults to local disk under `hub_dir`
    /// when unset; the composition root wires S3/MinIO when configured.
    pub storage: Option<Arc<dyn coxagent_application::ports::outbound::StoragePort>>,
    /// Server-side documentation store (e.g. MongoDB). `None` = per-project state.
    pub doc_store: Option<Arc<dyn coxagent_application::ports::outbound::DocStorePort>>,
    /// Shared KV store for hub-wide singletons (system chat). `None` = local file.
    pub syschat_store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

/// Assemble the shared [`AppState`] from the registered projects and hub extras.
/// Space budget ENFORCEMENT (not just display): every 5 minutes each space's
/// total spend is compared to its cap; the first breach pauses every runner and
/// registered operator of the space's projects and posts one notice to each
/// project's #agents. Re-arms when the cap is raised above the spend (or the
/// cap is removed) — so topping up the budget lets a Start actually stick.
async fn space_budget_watchdog(app: AppState) {
    let mut flagged: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut warned: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Matches the dashboard's own amber threshold (index.html renders the
    // "nearly reached" alert at 80% of a project's cap) — same UX language,
    // just at the space level and pushed as a chat heads-up.
    const WARN_PCT: f64 = 0.8;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        let spaces = app.spaces.inner.lock().await.spaces.clone();
        for sp in spaces {
            if sp.budget_usd <= 0.0 {
                flagged.remove(&sp.id);
                warned.remove(&sp.id);
                continue;
            }
            let handles: Vec<ProjectHandle> = {
                let projects = app.projects.read().await;
                sp.projects
                    .iter()
                    .filter_map(|pid| projects.get(pid).cloned())
                    .collect()
            };
            let mut spend = 0.0;
            for p in &handles {
                if let Ok(st) = p.store.load().await {
                    spend += st.spend.total_cost_usd;
                }
            }
            if spend < sp.budget_usd {
                flagged.remove(&sp.id);
            }
            // Early warning ahead of the hard stop below: fires once per
            // approach toward the cap, clears once spend drops back out of
            // the warning band (cap raised, or the hard stop below already
            // took over) so a later crossing can warn again.
            if coxagent_application::policy::approaching_cap(spend, Some(sp.budget_usd), WARN_PCT) {
                if warned.insert(sp.id.clone()) {
                    let msg = format!(
                        "⚠️ BUDGET: space \"{}\" đã đốt ${spend:.2} / cap ${:.2} ({:.0}%) — sắp \
                         chạm mức dừng tự động. Nâng budget (Manage → Edit space) nếu muốn team \
                         tiếp tục chạy liên tục.",
                        sp.name,
                        sp.budget_usd,
                        spend / sp.budget_usd * 100.0
                    );
                    for p in &handles {
                        let _ = coxagent_application::ports::outbound::mutate_state(
                            p.store.as_ref(),
                            |s| {
                                s.post_chat_in(
                                    "COX",
                                    &msg,
                                    coxagent_application::state::AGENTS_CHANNEL,
                                    Vec::new(),
                                );
                                s.log_activity(
                                    "COX",
                                    "space budget approaching cap — warned",
                                    None,
                                );
                                Ok(())
                            },
                        )
                        .await;
                    }
                }
            } else {
                warned.remove(&sp.id);
            }
            if spend < sp.budget_usd {
                continue;
            }
            if !flagged.insert(sp.id.clone()) {
                continue; // already enforced for this breach
            }
            tracing::warn!(
                "space {} over budget (${spend:.2} >= ${:.2}) — pausing its agents",
                sp.id,
                sp.budget_usd
            );
            let msg = format!(
                "⛔ BUDGET: space \"{}\" đã đốt ${spend:.2} / cap ${:.2} — toàn bộ agent của \
                 space bị TẠM DỪNG. Super Admin nâng budget (Manage → Edit space) rồi Start lại \
                 để tiếp tục.",
                sp.name, sp.budget_usd
            );
            for p in &handles {
                p.runner.pause();
                if let Ok(workers) = p.store.workers().await {
                    for w in workers {
                        let _ = p.store.set_desired(&w.worker, false).await;
                    }
                }
                let _ =
                    coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
                        s.post_chat_in(
                            "COX",
                            &msg,
                            coxagent_application::state::AGENTS_CHANNEL,
                            Vec::new(),
                        );
                        s.log_activity("COX", "space budget cap reached — agents paused", None);
                        Ok(())
                    })
                    .await;
            }
        }
    }
}

/// Embedded terminal (IDE-style): a real PTY in the project's codebase dir,
/// bridged over this WebSocket. Arbitrary shell = full host access, so the
/// gate is hard: Admin/Super only, and every session start is audited.
/// Protocol: client sends JSON text frames {"input": "..."} and
/// {"resize": {"cols": N, "rows": N}}; server sends raw output as binary.
async fn terminal_ws_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Some(resp) = role_guard(true) {
        return resp;
    }
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    // Every working member gets a shell (like opening the OS terminal) — only
    // the read-only legacy Viewer is excluded. Each session start is audited.
    let user = match &app.auth {
        Some(auth) => match resolve_principal(auth, &headers).await {
            Some(u) if u.role.can_write() => u.username,
            Some(_) => {
                return (StatusCode::FORBIDDEN, "read-only role").into_response();
            }
            None => return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response(),
        },
        None => "user".to_owned(),
    };
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Execution-plane guard: a hardened control-plane deployment (K8s gateway)
    // sets COXAGENT_NO_INLINE_EXEC=1 — no shells in this process, ever.
    if std::env::var("COXAGENT_NO_INLINE_EXEC").is_ok_and(|v| v == "1") {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "terminals are disabled on the control plane — connect via a runner",
        )
            .into_response();
    }
    app.audit
        .record(coxagent_application::ports::outbound::AuditRecord {
            at: coxagent_application::state::now_rfc3339(),
            user: user.clone(),
            action: format!("TERMINAL open {pid}"),
            status: 101,
        })
        .await;
    let work_dir = p.work_dir.clone();
    ws.max_message_size(256 * 1024)
        .on_upgrade(move |socket| terminal_socket(socket, work_dir, user, pid))
}

async fn terminal_socket(mut socket: WebSocket, work_dir: PathBuf, user: String, pid: String) {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    let pty = native_pty_system();
    let Ok(pair) = pty.openpty(PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    }) else {
        let _ = socket
            .send(Message::Text("\r\n[pty unavailable]\r\n".into()))
            .await;
        return;
    };
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-l");
    cmd.env("TERM", "xterm-256color");
    if work_dir.is_dir() {
        cmd.cwd(&work_dir);
    }
    let Ok(mut child) = pair.slave.spawn_command(cmd) else {
        let _ = socket
            .send(Message::Text("\r\n[shell spawn failed]\r\n".into()))
            .await;
        return;
    };
    drop(pair.slave);
    let Ok(mut reader) = pair.master.try_clone_reader() else {
        return;
    };
    let Ok(mut writer) = pair.master.take_writer() else {
        return;
    };
    tracing::info!("terminal session opened by {user} on {pid}");
    // Blocking PTY reads → channel → WS.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let master = pair.master;
    loop {
        tokio::select! {
            out = rx.recv() => {
                if let Some(bytes) = out {
                    if socket.send(Message::Binary(bytes)).await.is_err() { break; }
                } else { // shell exited
                    let _ = socket.send(Message::Text("\r\n[session ended]\r\n".into())).await;
                    break;
                }
            }
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                if let Message::Text(t) = msg {
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else { continue };
                    if let Some(input) = v.get("input").and_then(|x| x.as_str()) {
                        use std::io::Write;
                        if writer.write_all(input.as_bytes()).is_err() { break; }
                        let _ = writer.flush();
                    } else if let Some(r) = v.get("resize") {
                        let cols = r.get("cols").and_then(serde_json::Value::as_u64).unwrap_or(100);
                        let rows = r.get("rows").and_then(serde_json::Value::as_u64).unwrap_or(30);
                        #[allow(clippy::cast_possible_truncation)]
                        let _ = master.resize(PtySize {
                            rows: rows.clamp(4, 300) as u16,
                            cols: cols.clamp(20, 500) as u16,
                            pixel_width: 0, pixel_height: 0,
                        });
                    }
                }
            }
        }
    }
    let _ = child.kill();
    tracing::info!("terminal session closed ({user} on {pid})");
}

/// Which surface this process serves — the physical service split. One binary,
/// four roles (`COXAGENT_ROLE`): `all` (default, self-host single process),
/// `gateway` (REST + MCP, no sockets), `realtime` (WS/SSE only), `knowledge`
/// (batch loops only). A load balancer routes paths to the right pods; the
/// role guard makes serving the wrong surface impossible, not just unrouted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HubRole {
    All,
    Gateway,
    Realtime,
    Knowledge,
}

fn hub_role() -> HubRole {
    static ROLE: std::sync::OnceLock<HubRole> = std::sync::OnceLock::new();
    *ROLE.get_or_init(
        || match std::env::var("COXAGENT_ROLE").unwrap_or_default().as_str() {
            "gateway" => HubRole::Gateway,
            "realtime" => HubRole::Realtime,
            "knowledge" => HubRole::Knowledge,
            _ => HubRole::All,
        },
    )
}

/// 503 unless this process's role serves the given surface.
fn role_guard(need_realtime: bool) -> Option<axum::response::Response> {
    let ok = match hub_role() {
        HubRole::All => true,
        HubRole::Gateway => !need_realtime,
        HubRole::Realtime => need_realtime,
        HubRole::Knowledge => false,
    };
    if ok {
        None
    } else {
        Some(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "wrong service role for this endpoint — check the load balancer routing",
            )
                .into_response(),
        )
    }
}

/// MCP server (streamable-HTTP JSON-RPC) — the gateway's third transport
/// beside REST and WS. Engines and MCP clients (claude CLI, Claude Desktop,
/// Cursor) PULL exactly the context they need instead of being fed capped
/// prompt blocks. Same authz as REST (session cookie or API token via the
/// auth middleware); every tools/call is audited.
async fn mcp_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> axum::response::Response {
    let id = req.get("id").cloned();
    let method = req
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let rpc = |id: Option<serde_json::Value>, result: serde_json::Value| {
        Json(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
    };
    let rpc_err = |id: Option<serde_json::Value>, code: i64, msg: &str| {
        Json(serde_json::json!({ "jsonrpc": "2.0", "id": id,
            "error": { "code": code, "message": msg } }))
        .into_response()
    };
    match method {
        "initialize" => rpc(
            id,
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "coxagent", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),
        "notifications/initialized" | "notifications/cancelled" => {
            StatusCode::ACCEPTED.into_response()
        }
        "ping" => rpc(id, serde_json::json!({})),
        "tools/list" => rpc(id, serde_json::json!({ "tools": mcp_tool_specs() })),
        "tools/call" => {
            let user = resolve_username(&app, &headers).await;
            let name = req
                .pointer("/params/name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let args = req
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            audit_push(
                &app.audit,
                &user,
                format!(
                    "MCP {name} {}",
                    args.get("project")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                ),
                200,
            )
            .await;
            match mcp_call(&app, name, &args).await {
                Ok(text) => rpc(
                    id,
                    serde_json::json!({ "content": [{ "type": "text", "text": text }] }),
                ),
                Err(msg) => rpc(
                    id,
                    serde_json::json!({ "content": [{ "type": "text", "text": msg }],
                        "isError": true }),
                ),
            }
        }
        _ => rpc_err(id, -32601, "method not found"),
    }
}

/// The MCP tool catalogue — kept small and high-signal on purpose.
fn mcp_tool_specs() -> serde_json::Value {
    let proj = serde_json::json!({ "type": "string", "description": "project id, e.g. cxc" });
    serde_json::json!([
        { "name": "search_symbols",
          "description": "Search the project's code graph for symbols/files relevant to a query — use before reading code blindly.",
          "inputSchema": { "type": "object", "properties": { "project": proj, "query": { "type": "string" } }, "required": ["project", "query"] } },
        { "name": "symbol_refs",
          "description": "All references, callers and callees of a symbol — impact analysis before changing it.",
          "inputSchema": { "type": "object", "properties": { "project": proj, "name": { "type": "string" } }, "required": ["project", "name"] } },
        { "name": "get_ticket",
          "description": "Full ticket detail: description, acceptance criteria, technical/UX design, status.",
          "inputSchema": { "type": "object", "properties": { "project": proj, "id": { "type": "string" } }, "required": ["project", "id"] } },
        { "name": "pr_queue",
          "description": "Open pull requests with mergeable/CI state — check before opening a new branch.",
          "inputSchema": { "type": "object", "properties": { "project": proj }, "required": ["project"] } },
        { "name": "report_blocker",
          "description": "Report a blocker to the team's #agents channel so a human sees it.",
          "inputSchema": { "type": "object", "properties": { "project": proj, "note": { "type": "string" } }, "required": ["project", "note"] } }
    ])
}

/// Dispatch one MCP tool call onto the SAME internals the REST API uses.
async fn mcp_call(app: &AppState, name: &str, args: &serde_json::Value) -> Result<String, String> {
    let pid = args
        .get("project")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing 'project'")?;
    let p = app.project(pid).await.ok_or("unknown project")?;
    let s = |k: &str| {
        args.get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    match name {
        "search_symbols" => {
            let q = s("query").ok_or("missing 'query'")?;
            let g = coxagent_application::codegraph::CodeGraph::load(&p.work_dir)
                .ok_or("code graph not built yet")?;
            serde_json::to_string_pretty(&g.relevance_search(q, 30)).map_err(|e| e.to_string())
        }
        "symbol_refs" => {
            let sym = s("name").ok_or("missing 'name'")?.to_owned();
            let wd = p.work_dir.clone();
            let refs = tokio::task::spawn_blocking({
                let (wd, sym) = (wd.clone(), sym.clone());
                move || coxagent_application::codegraph::references(&wd, &sym, 100)
            })
            .await
            .unwrap_or_default();
            let (inbound, outbound) = coxagent_application::codegraph::CodeGraph::load(&wd)
                .map(|g| (g.callers(&sym), g.callees(&sym)))
                .unwrap_or_default();
            serde_json::to_string_pretty(&serde_json::json!({
                "refs": refs, "callers": inbound, "callees": outbound
            }))
            .map_err(|e| e.to_string())
        }
        "get_ticket" => {
            let tid = s("id").ok_or("missing 'id'")?;
            let state = p.store.load().await.map_err(|e| e.to_string())?;
            let t = state
                .tickets
                .iter()
                .find(|t| t.id().to_string() == tid)
                .ok_or("ticket not found")?;
            serde_json::to_string_pretty(t).map_err(|e| e.to_string())
        }
        "pr_queue" => {
            let forge = p.forge.clone().ok_or("no forge configured")?;
            let prs = forge.list_open_prs().await.map_err(|e| e.to_string())?;
            let rows: Vec<_> = prs
                .iter()
                .map(|x| {
                    serde_json::json!({ "number": x.number, "title": x.title,
                        "mergeable": x.mergeable, "ci": x.ci, "created": x.created })
                })
                .collect();
            serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())
        }
        "report_blocker" => {
            let note = s("note").ok_or("missing 'note'")?.to_owned();
            coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |st| {
                st.post_chat_in(
                    "ENGINE",
                    &format!("🚧 Blocker: {note}"),
                    coxagent_application::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
            Ok("reported to #agents".to_owned())
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

/// Bridge the in-process chat broadcast onto Redis pub/sub (`cox:events`), so
/// every hub instance sees every event — the piece that makes the gateway
/// horizontally scalable. Loop safety: outbound frames carry this instance's
/// origin id (dropped by our own subscriber), and payloads just received from
/// the bus are remembered briefly so re-broadcasting them locally doesn't
/// publish an echo back.
async fn redis_bus_bridge(app: AppState, url: String) {
    use coxagent_contracts::BusEnvelope;
    let origin = format!("hub-{}", std::process::id());
    let recent: Arc<std::sync::Mutex<std::collections::VecDeque<String>>> =
        Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let remember = |recent: &std::sync::Mutex<std::collections::VecDeque<String>>, s: &str| {
        if let Ok(mut q) = recent.lock() {
            q.push_back(s.to_owned());
            while q.len() > 256 {
                q.pop_front();
            }
        }
    };
    let seen = |recent: &std::sync::Mutex<std::collections::VecDeque<String>>, s: &str| {
        recent.lock().is_ok_and(|q| q.iter().any(|x| x == s))
    };
    // Outbound: local broadcast → Redis.
    {
        let url = url.clone();
        let origin = origin.clone();
        let recent = Arc::clone(&recent);
        let tx = app.syschat.tx.clone();
        tokio::spawn(async move {
            loop {
                let Ok(client) = redis::Client::open(url.as_str()) else {
                    return;
                };
                let Ok(mut conn) = client.get_multiplexed_async_connection().await else {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    continue;
                };
                let mut rx = tx.subscribe();
                while let Ok(payload) = rx.recv().await {
                    if seen(&recent, &payload) {
                        continue; // just came FROM the bus — don't echo it back
                    }
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) else {
                        continue;
                    };
                    let env = BusEnvelope::new(&origin, "syschat", v);
                    if let Ok(frame) = serde_json::to_string(&env) {
                        let _: Result<(), _> =
                            redis::AsyncCommands::publish(&mut conn, "cox:events", frame).await;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }
    // Inbound: Redis → local broadcast.
    loop {
        let Ok(client) = redis::Client::open(url.as_str()) else {
            return;
        };
        let Ok(mut pubsub) = client.get_async_pubsub().await else {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        };
        if pubsub.subscribe("cox:events").await.is_err() {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }
        let mut stream = pubsub.on_message();
        while let Some(msg) = futures_util::StreamExt::next(&mut stream).await {
            let frame: String = match msg.get_payload() {
                Ok(f) => f,
                Err(_) => continue,
            };
            let Ok(env) = serde_json::from_str::<BusEnvelope>(&frame) else {
                continue;
            };
            if env.origin == origin || env.v != coxagent_contracts::CONTRACT_VERSION {
                continue;
            }
            if let Ok(payload) = serde_json::to_string(&env.payload) {
                remember(&recent, &payload);
                let _ = app.syschat.tx.send(payload);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Compose-project prefix of a PR preview (`<workspace>/.preview/<num>` via
/// `compose_project_name`). Previews are meant to live for as long as someone
/// is looking at them.
const PREVIEW_PROJECT_PREFIX: &str = "cox--preview-";
/// How long a PR preview may stay up before the janitor reclaims it. Someone
/// opens a preview, reads the diff, and walks away — without this, the
/// container holds the app port and its share of the host for good. (One was
/// found still running after eight days.)
const PREVIEW_TTL: &str = "6h";

/// Docker janitor: agents deploy a lot — the host must not silt up. Hourly:
/// any `cox-*` compose project whose containers are ALL stopped gets a full
/// `down --remove-orphans` (dead previews, stale deploys), any PR preview
/// still running past [`PREVIEW_TTL`] is reclaimed, then dangling build images
/// are pruned. Scoped strictly to the `cox-` prefix — the backing-services
/// group (`cox-infra`) is running, so it is never touched, and a RUNNING
/// non-preview project is someone's live deploy and is left alone.
async fn docker_janitor() {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        let Ok(out) = tokio::process::Command::new("docker")
            .args(["compose", "ls", "-a", "--format", "json"])
            .stdin(std::process::Stdio::null())
            .output()
            .await
        else {
            continue;
        };
        let Ok(list) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) else {
            continue;
        };
        for p in &list {
            let name = p
                .get("Name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let status = p
                .get("Status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            // Only OUR projects, and only fully-stopped ones.
            if !name.starts_with("cox-") || name == "cox-infra" {
                continue;
            }
            let mut reason = "dead";
            if status.contains("running") {
                if !name.starts_with(PREVIEW_PROJECT_PREFIX)
                    || !preview_is_stale(name, PREVIEW_TTL).await
                {
                    continue;
                }
                reason = "expired preview";
            }
            let _ = tokio::process::Command::new("docker")
                .args(["compose", "-p", name, "down", "--remove-orphans"])
                .stdin(std::process::Stdio::null())
                .output()
                .await;
            tracing::info!("docker janitor: removed {reason} compose project {name}");
        }
        let _ = tokio::process::Command::new("docker")
            .args(["image", "prune", "-f"])
            .stdin(std::process::Stdio::null())
            .output()
            .await;
    }
}

/// Whether a preview project has a container created longer ago than `ttl`.
/// Docker's own `until` filter does the age arithmetic, so no timestamp
/// parsing (and no timezone bug) of ours stands between a forgotten preview
/// and being reclaimed.
async fn preview_is_stale(project: &str, ttl: &str) -> bool {
    let Ok(out) = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-q",
            "--filter",
            &format!("label=com.docker.compose.project={project}"),
            "--filter",
            &format!("until={ttl}"),
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return false;
    };
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

/// Disaster-recovery floor for the hub-level app_kv documents: once a day,
/// snapshot workspace + spaces + system chat as dated JSON under
/// `<hub_dir>/backups/YYYY-MM-DD/`, pruning snapshots older than 14 days.
/// Restore = copy a snapshot back over the store (documented in DEPLOYMENT.md).
async fn nightly_backup(app: AppState, dir: PathBuf) {
    loop {
        let day = coxagent_application::state::now_rfc3339()[..10].to_owned();
        let dest = dir.join(&day);
        let done = dest.join("spaces.json").exists();
        if !done {
            let _ = std::fs::create_dir_all(&dest);
            let ws = app.workspace.inner.lock().await.clone();
            let sp = app.spaces.inner.lock().await.clone();
            let chat = app.syschat.inner.lock().await.clone();
            let dump = |name: &str, v: serde_json::Result<String>| {
                if let Ok(text) = v {
                    let _ = std::fs::write(dest.join(name), text);
                }
            };
            dump("workspace.json", serde_json::to_string_pretty(&ws));
            dump("spaces.json", serde_json::to_string_pretty(&sp));
            dump("system_chat.json", serde_json::to_string_pretty(&chat));
            tracing::info!("nightly backup written to {}", dest.display());
            // Prune snapshots older than 14 days (lexicographic = chronologic).
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut days: Vec<String> = entries
                    .filter_map(Result::ok)
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect();
                days.sort();
                while days.len() > 14 {
                    let old = days.remove(0);
                    let _ = std::fs::remove_dir_all(dir.join(old));
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

async fn build_state(
    projects: Vec<ProjectHandle>,
    audit: Arc<dyn AuditPort>,
    extras: HubExtras,
) -> AppState {
    let order: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();
    let map: HashMap<String, ProjectHandle> =
        projects.into_iter().map(|p| (p.id.clone(), p)).collect();
    let hub_dir = extras.hub_dir.unwrap_or_else(|| PathBuf::from("."));
    let kv = extras.syschat_store.clone();
    let kv2 = extras.syschat_store.clone();
    let kv3 = extras.syschat_store.clone();
    let kv3_pf = extras.syschat_store.clone();
    let syschat = SysChat::load(&hub_dir, extras.syschat_store).await;
    let workspace = Ws::load(&hub_dir, kv).await;
    let spaces = Sp::load(&hub_dir, kv2).await;
    let kv4 = kv3_pf.clone();
    let meetings = Mt::load(&hub_dir, kv3).await;
    let profiles = Pf::load(&hub_dir, kv4).await;
    AppState {
        projects: Arc::new(RwLock::new(map)),
        chat_bus: Arc::new(RwLock::new(HashMap::new())),
        docs_bus: Arc::new(RwLock::new(HashMap::new())),
        docs_editors: Arc::new(std::sync::Mutex::new(HashMap::new())),
        order: Arc::new(RwLock::new(order)),
        factory: extras.factory,
        auth: extras.auth,
        audit,
        engines: Arc::new(extras.engines),
        tooling: Arc::new(extras.tooling),
        viewers: Arc::new(std::sync::Mutex::new(HashMap::new())),
        remover: extras.remover,
        analyzer: extras.analyzer,
        syschat,
        workspace,
        spaces,
        meetings,
        profiles,
        storage: extras.storage.unwrap_or_else(|| {
            Arc::new(DiskStorage {
                root: hub_dir.join("blobs"),
            })
        }),
        doc_store: extras.doc_store,
    }
}

/// Serve the dashboard and API on `port`, with the security-audit sink and the
/// optional hub capabilities in `extras`.
///
/// # Errors
/// Returns an IO error if the port cannot be bound.
// A flat registry of route → handler wiring; length is inherent, not complexity.
#[allow(clippy::too_many_lines)]
pub async fn serve_full(
    projects: Vec<ProjectHandle>,
    port: u16,
    audit: Arc<dyn AuditPort>,
    extras: HubExtras,
) -> std::io::Result<()> {
    let backup_dir = extras
        .hub_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("backups");
    let state = build_state(projects, audit, extras).await;
    tracing::info!("hub role: {:?}", hub_role());
    // Batch/watchdog loops belong to the knowledge role (and the all-in-one).
    if matches!(hub_role(), HubRole::All | HubRole::Knowledge) {
        // Space budget enforcement runs for the life of the hub.
        tokio::spawn(space_budget_watchdog(state.clone()));
        // Meeting reminders, start announcements, and absent-participant rings.
        tokio::spawn(meeting_watchdog(state.clone()));
        // Nightly snapshots of the hub-level documents (workspace, spaces, chat).
        tokio::spawn(nightly_backup(state.clone(), backup_dir));
        // App-release watcher: new tagged builds surface as update notices.
        tokio::spawn(releases_watchdog(state.clone()));
        // Keep the docker host clean of dead agent deploys.
        tokio::spawn(docker_janitor());
    }
    // Cross-instance realtime: bridge the local chat broadcast onto Redis
    // pub/sub so N hub instances fan out the same events (no-op without Redis).
    // Gateway needs it too — REST-posted chat must reach realtime pods.
    if !matches!(hub_role(), HubRole::Knowledge) {
        if let Ok(url) = std::env::var("COXAGENT_REDIS_URL") {
            if !url.trim().is_empty() {
                tokio::spawn(redis_bus_bridge(state.clone(), url));
            }
        }
    }

    let app = Router::new()
        .route("/", get(index))
        .route(
            "/assets/xterm.min.js",
            get(|| async { ([("content-type", "application/javascript")], XTERM_JS) }),
        )
        .route(
            "/assets/xterm.min.css",
            get(|| async { ([("content-type", "text/css")], XTERM_CSS) }),
        )
        .route(
            "/assets/xterm-addon-fit.min.js",
            get(|| async { ([("content-type", "application/javascript")], XTERM_FIT_JS) }),
        )
        .route("/api/health", get(health))
        .route("/api/mcp", post(mcp_ep))
        .route("/api/app/latest", get(app_latest_ep))
        .route("/api/app/download/:file", get(app_download_ep))
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
        .route(
            "/api/auth/my/tokens",
            get(my_tokens_ep).post(create_my_token_ep),
        )
        .route(
            "/api/meetings",
            get(meetings_list_ep).post(meeting_create_ep),
        )
        .route("/api/meetings/:id", axum::routing::patch(meeting_patch_ep))
        .route("/api/meetings/:id/join", post(meeting_join_ep))
        .route("/api/meetings/:id/ring", post(meeting_ring_ep))
        .route("/api/profiles", get(profiles_ep))
        .route("/api/profile", post(profile_set_ep))
        .route("/api/auth/profile", axum::routing::patch(self_profile_ep))
        .route(
            "/api/profile/avatar",
            post(profile_avatar_ep).delete(profile_avatar_clear_ep),
        )
        .route(
            "/api/auth/my/tokens/:label",
            axum::routing::delete(revoke_my_token_ep),
        )
        .route("/api/auth/2fa/enroll", post(enroll_2fa_ep))
        .route("/api/auth/2fa/enable", post(enable_2fa_ep))
        .route("/api/auth/2fa/disable", post(disable_2fa_ep))
        .route("/api/auth/users", get(list_users_ep).post(create_user_ep))
        .route(
            "/api/auth/users/:username",
            axum::routing::delete(delete_user_ep).patch(update_user_ep),
        )
        .route(
            "/api/auth/users/:username/password",
            post(reset_password_ep),
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
        // System-wide chat (hub-level, shared across projects).
        .route(
            "/api/chat/channels",
            get(syschat_channels_ep).post(syschat_create_ep),
        )
        .route("/api/chat/channels/:cid/invite", post(syschat_invite_ep))
        .route(
            "/api/chat/channel/:cid/topic",
            axum::routing::patch(syschat_topic_ep).get(syschat_topic_get_ep),
        )
        .route(
            "/api/chat/channels/:cid/topic",
            axum::routing::patch(syschat_topic_ep).get(syschat_topic_get_ep),
        )
        .route("/api/chat/messages", get(syschat_messages_ep))
        .route("/api/chat/members", get(syschat_members_ep))
        .route("/api/chat/dm", post(syschat_dm_ep))
        .route("/api/chat/react", post(syschat_react_ep))
        .route("/api/chat/messages/:mid/reply", post(syschat_reply_ep))
        .route("/api/chat/messages/:mid/thread", get(syschat_thread_ep))
        .route(
            "/api/chat/messages/:mid",
            axum::routing::patch(syschat_edit_ep).delete(syschat_delete_ep),
        )
        .route("/api/chat/search", get(syschat_search_ep))
        .route("/api/chat/messages/:mid/pin", post(syschat_pin_ep))
        .route("/api/chat/pins", get(syschat_pins_ep))
        .route(
            "/api/chat/webhooks",
            get(syschat_webhooks_list_ep).post(syschat_webhook_create_ep),
        )
        .route(
            "/api/chat/webhooks/:token",
            axum::routing::delete(syschat_webhook_delete_ep),
        )
        .route("/api/chat/hook/:token", post(syschat_hook_ep))
        .route("/api/chat/ice", get(ice_config_ep))
        .route("/api/chat/send", post(syschat_send_ep))
        .route("/api/chat/ws", get(syschat_ws_ep))
        .route("/api/chat/upload", post(syschat_upload_ep))
        .route("/api/chat/media/:file", get(syschat_media_ep))
        .route("/api/engines", get(engines_ep))
        .route("/api/engines/opencode/models", get(opencode_models_ep))
        .route("/api/tooling", get(tooling_ep))
        .route("/api/analyze-goal", post(analyze_goal_ep))
        .route("/api/projects", get(list_projects).post(create_project))
        .route(
            "/api/projects/:pid",
            axum::routing::delete(delete_project_ep).patch(rename_project_ep),
        )
        .route("/api/projects/:pid/state", get(state_ep))
        .route("/api/projects/:pid/metrics", get(metrics_ep))
        .route("/api/projects/:pid/agent-evals", get(agent_evals_ep))
        .route("/api/projects/:pid/runner", get(runner_ep))
        .route("/api/projects/:pid/workers", get(workers_ep))
        .route("/api/token-saver", get(token_saver_ep))
        .route("/api/projects/:pid/audit", get(audit_ep))
        .route("/api/projects/:pid/config", get(get_config).put(put_config))
        .route("/api/projects/:pid/control/:action", post(control_ep))
        .route("/api/projects/:pid/sprint/goal", post(set_sprint_goal_ep))
        .route("/api/projects/:pid/digest", post(digest_ep))
        .route("/api/projects/:pid/merge-sweep", post(merge_sweep_ep))
        .route(
            "/api/workspace",
            get(workspace_get_ep).put(workspace_put_ep),
        )
        .route(
            "/api/workspace/invites",
            get(invites_list_ep).post(invite_create_ep),
        )
        .route("/api/workspace/invites/:token", delete(invite_delete_ep))
        .route("/api/workspace/overview", get(workspace_overview_ep))
        .route("/api/spaces", get(spaces_list_ep).post(space_create_ep))
        .route(
            "/api/spaces/:sid",
            axum::routing::put(space_update_ep).delete(space_delete_ep),
        )
        .route("/api/manage/overview", get(manage_overview_ep))
        .route("/api/manage/spaces/:sid", get(manage_space_detail_ep))
        .route("/api/me/agents", get(my_agents_ep))
        .route("/join/:token", get(join_page_ep))
        .route("/api/workspace/join", post(join_ep))
        .route(
            "/api/projects/:pid/operators/:operator/:action",
            post(operator_control_ep),
        )
        .route("/api/projects/:pid/ba-analyze", post(ba_analyze))
        .route("/api/projects/:pid/ticket-refine", post(ticket_refine))
        .route("/api/projects/:pid/discuss", post(run_discussion_ep))
        .route("/api/projects/:pid/docs", get(docs_list_ep))
        .route("/api/projects/:pid/docs/generate", post(docs_generate_ep))
        .route(
            "/api/projects/:pid/doc-folders",
            get(doc_folders_ep).post(doc_folder_add_ep),
        )
        .route(
            "/api/projects/:pid/doc-folders/delete",
            post(doc_folder_del_ep),
        )
        .route("/api/projects/:pid/docs/:id/move", post(doc_move_ep))
        .route(
            "/api/projects/:pid/docs/:id",
            axum::routing::put(doc_upsert_ep).delete(doc_delete_ep),
        )
        .route("/api/projects/:pid/docs/:id/ai-edit", post(doc_ai_edit_ep))
        .route("/api/projects/:pid/docs/:id/ws", get(docs_ws_ep))
        .route("/api/projects/:pid/terminal", get(terminal_ws_ep))
        .route("/api/projects/:pid/codegraph", get(codegraph_ep))
        .route("/api/projects/:pid/codegraph/refs", get(codegraph_refs_ep))
        .route("/api/projects/:pid/codegraph/deps", get(codegraph_deps_ep))
        .route(
            "/api/projects/:pid/codegraph/build",
            post(codegraph_build_ep),
        )
        .route("/api/projects/:pid/standup", post(standup_ep))
        .route(
            "/api/projects/:pid/architecture-review",
            post(architecture_review_ep),
        )
        .route("/api/projects/:pid/docs-review", post(docs_review_ep))
        .route("/api/projects/:pid/chat-reply", post(chat_reply_ep))
        .route("/api/projects/:pid/tickets", post(create_ticket))
        .route("/api/projects/:pid/ticket/:id", get(ticket_detail_ep))
        .route("/api/projects/:pid/ticket/:id/priority", post(set_priority))
        .route("/api/projects/:pid/ticket/:id/reject", post(reject_ticket))
        .route(
            "/api/projects/:pid/ticket/:id/approve-cost",
            post(approve_cost),
        )
        .route("/api/projects/:pid/ticket/:id/unpark", post(unpark_ticket))
        .route("/api/projects/:pid/ticket/:id/edit", post(edit_ticket))
        .route(
            "/api/projects/:pid/comments",
            get(list_comments).post(post_comment),
        )
        .route(
            "/api/projects/:pid/comments/:id/react",
            post(comment_react_ep),
        )
        .route(
            "/api/projects/:pid/chat",
            get(chat_list_ep).post(chat_post_ep),
        )
        .route("/api/projects/:pid/chat/ws", get(chat_ws_ep))
        .route(
            "/api/projects/:pid/channels",
            get(channels_list_ep).post(channel_create_ep),
        )
        .route(
            "/api/projects/:pid/channels/:cid/invite",
            post(channel_invite_ep),
        )
        .route("/api/projects/:pid/upload", post(upload_ep))
        .route("/api/projects/:pid/media/:file", get(media_ep))
        .route(
            "/api/projects/:pid/context",
            get(context_ep).post(context_update_ep),
        )
        .route("/api/projects/:pid/git/auth", get(git_auth_status_ep))
        .route("/api/projects/:pid/git/connect", post(git_connect_ep))
        .route("/api/projects/:pid/git/test", post(git_test_ep))
        .route("/api/projects/:pid/prs", get(list_prs_ep))
        .route("/api/projects/:pid/prs/:num/diff", get(pr_diff_ep))
        .route("/api/projects/:pid/prs/:num/:action", post(pr_action_ep))
        .route("/api/projects/:pid/agent-log", get(agent_log_ep))
        .route("/api/projects/:pid/transcripts", get(list_transcripts))
        .route("/api/projects/:pid/transcripts/:name", get(get_transcript))
        .route("/api/projects/:pid/events", get(events_ep))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))
        .layer(axum::middleware::from_fn(security_headers_mw))
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
    //
    // CSP + hardening headers. The dashboard uses inline <script>/<style> (a
    // single embedded file) so 'unsafe-inline' is required there; the Inter font
    // and Tabler icon webfont come from Google Fonts / jsDelivr, so those hosts
    // are allow-listed for style/font. Everything else is locked to same-origin,
    // WebSocket to self, images/fonts to data:, and framing is denied.
    const CSP: &str = "default-src 'self'; \
        script-src 'self' 'unsafe-inline'; \
        style-src 'self' 'unsafe-inline' https://fonts.googleapis.com https://cdn.jsdelivr.net; \
        font-src 'self' data: https://fonts.gstatic.com https://cdn.jsdelivr.net; \
        img-src 'self' data:; \
        connect-src 'self' ws: wss:; \
        object-src 'none'; base-uri 'self'; frame-ancestors 'none'";
    (
        [
            (header::CACHE_CONTROL, "no-cache, must-revalidate"),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::REFERRER_POLICY, "strict-origin-when-cross-origin"),
        ],
        Html(INDEX_HTML),
    )
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// What clients need to offer downloads + update notices: the hub's own
/// version, the newest published app version, and per-platform URLs.
async fn app_latest_ep(State(app): State<AppState>) -> impl IntoResponse {
    let d = app.workspace.inner.lock().await.downloads.clone();
    Json(serde_json::json!({
        "hub_version": env!("CARGO_PKG_VERSION"),
        "latest_version": d.latest_version,
        "downloads": {
            "macos": d.macos, "windows": d.windows, "linux": d.linux, "ios": d.ios,
        },
        "notes": d.notes,
        "releases_repo": d.releases_repo,
    }))
}

/// Stream a release asset through the hub — the repo may be PRIVATE, so
/// clients can't hit GitHub's download URLs anonymously; the hub's `gh` auth
/// does it server-side and the token never leaves this process. Public path
/// (it serves the installer, same trust as the login page); path ends in the
/// real file extension so the native updater's checks hold.
async fn app_download_ep(
    State(app): State<AppState>,
    Path(file): Path<String>,
) -> axum::response::Response {
    let (repo, ver) = {
        let d = &app.workspace.inner.lock().await.downloads;
        (d.releases_repo.trim().to_owned(), d.latest_version.clone())
    };
    if repo.is_empty() || ver.is_empty() {
        return (StatusCode::NOT_FOUND, "no release configured").into_response();
    }
    let want: &[&str] = match file.as_str() {
        "macos.dmg" => &[".dmg"],
        "windows.exe" => &[".exe", ".msi", "windows-x64.zip"],
        "linux.tar.gz" => &["linux-x64.tar.gz", "linux.tar.gz", ".appimage", ".deb"],
        _ => return (StatusCode::NOT_FOUND, "unknown platform").into_response(),
    };
    let tag = format!("v{ver}");
    let Ok(meta) = tokio::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo}/releases/tags/{tag}"),
            "--jq",
            "[.assets[] | {id, name}]",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return internal_error("gh unavailable");
    };
    let assets: Vec<serde_json::Value> = serde_json::from_slice(&meta.stdout).unwrap_or_default();
    let found = assets.iter().find(|a| {
        a.get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|n| {
                let n = n.to_lowercase();
                want.iter().any(|w| n.ends_with(w))
            })
    });
    let Some(asset) = found else {
        return (StatusCode::NOT_FOUND, "no asset for this platform").into_response();
    };
    let id = asset
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let name = asset
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("download")
        .to_owned();
    let Ok(bin) = tokio::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo}/releases/assets/{id}"),
            "-H",
            "Accept: application/octet-stream",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return internal_error("asset fetch failed");
    };
    if !bin.status.success() || bin.stdout.len() < 1024 {
        return internal_error("asset fetch failed");
    }
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        bin.stdout,
    )
        .into_response()
}

/// Poll GitHub Releases (via the `gh` CLI already required for the forge) every
/// 30 minutes and refresh version + per-platform asset URLs — so a release
/// tagged by CI shows up as an update notice in every client, no manual step.
/// Manual URLs in the config win over auto-detected assets.
async fn releases_watchdog(app: AppState) {
    loop {
        let repo = app
            .workspace
            .inner
            .lock()
            .await
            .downloads
            .releases_repo
            .clone();
        if !repo.trim().is_empty() {
            let out = tokio::process::Command::new("gh")
                .args([
                    "api",
                    &format!("repos/{}/releases/latest", repo.trim()),
                    "--jq",
                    "{tag: .tag_name, body: .body, assets: [.assets[] | {name, url: .browser_download_url}]}",
                ])
                .stdin(std::process::Stdio::null())
                .output()
                .await;
            if let Ok(o) = out {
                if o.status.success() {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
                        let tag = v
                            .get("tag")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .trim_start_matches('v')
                            .to_owned();
                        let pick = |exts: &[&str]| -> String {
                            v.get("assets")
                                .and_then(serde_json::Value::as_array)
                                .and_then(|a| {
                                    a.iter().find(|x| {
                                        x.get("name")
                                            .and_then(serde_json::Value::as_str)
                                            .is_some_and(|n| {
                                                let n = n.to_lowercase();
                                                exts.iter().any(|e| n.ends_with(e))
                                            })
                                    })
                                })
                                .and_then(|x| x.get("url").and_then(serde_json::Value::as_str))
                                .unwrap_or("")
                                .to_owned()
                        };
                        let (dmg, exe, lin) = (
                            pick(&[".dmg"]),
                            pick(&[".exe", ".msi", "windows-x64.zip"]),
                            pick(&[".appimage", ".deb", "linux-x64.tar.gz", "linux.tar.gz"]),
                        );
                        let mut doc = app.workspace.inner.lock().await;
                        let d = &mut doc.downloads;
                        let changed = !tag.is_empty() && d.latest_version != tag;
                        if !tag.is_empty() {
                            d.latest_version = tag;
                        }
                        d.notes = v
                            .get("body")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(1500)
                            .collect();
                        // Auto-fill only where no manual override exists.
                        if d.macos.is_empty() || d.macos.contains("/releases/") {
                            d.macos = dmg;
                        }
                        if d.windows.is_empty() || d.windows.contains("/releases/") {
                            d.windows = exe;
                        }
                        if d.linux.is_empty() || d.linux.contains("/releases/") {
                            d.linux = lin;
                        }
                        drop(doc);
                        if changed {
                            app.workspace.save().await;
                            tracing::info!("app release refreshed from {repo}");
                        }
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
    }
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

/// Detect opencode models from the live CLI: runs `opencode models`, parses
/// output into provider/model pairs. Returns empty list on any failure.
async fn opencode_models_ep() -> impl IntoResponse {
    let output = tokio::process::Command::new("opencode")
        .arg("models")
        .output()
        .await;
    let stdout = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Json(Vec::<serde_json::Value>::new()).into_response(),
    };
    let mut models = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.contains("No models") || line.contains("Error") {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '/').collect();
        if parts.len() == 2 {
            models
                .push(serde_json::json!({ "provider": parts[0], "model": parts[1], "full": line }));
        }
    }
    Json(models).into_response()
}

/// Developer tooling the git/deploy flow needs (git, gh, glab, docker) — which
/// are installed, and how to install the rest. Computed at startup and injected.
async fn tooling_ep(State(app): State<AppState>) -> impl IntoResponse {
    Json((*app.tooling).clone())
}

/// The CLI + host env var for a git provider.
fn git_cli(provider: &str) -> (&'static str, &'static str) {
    if provider == "gitlab" {
        ("glab", "GITLAB_HOST")
    } else {
        ("gh", "GH_HOST")
    }
}

/// Run `bin args...` (no stdin), returning `(success, combined stdout+stderr)`.
async fn run_cli(bin: &str, args: &[&str]) -> (bool, String) {
    run_cli_env(bin, args, "", "").await
}

/// Like [`run_cli`], but sets one env var (e.g. `GITLAB_HOST`) when `key` is set.
async fn run_cli_env(bin: &str, args: &[&str], key: &str, val: &str) -> (bool, String) {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args).stdin(std::process::Stdio::null());
    if !key.is_empty() {
        cmd.env(key, val);
    }
    let out = cmd.output().await;
    match out {
        Ok(o) => (
            o.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        ),
        Err(_) => (false, format!("{bin} not found")),
    }
}

/// Pull the signed-in account out of `gh`/`glab auth status` output.
fn parse_account(out: &str) -> Option<String> {
    for marker in ["account ", " as ", "Logged in to "] {
        if let Some(i) = out.find(marker) {
            let rest = &out[i + marker.len()..];
            // Skip a leading host token for the "Logged in to" case.
            let name: String = rest
                .split_whitespace()
                .find(|w| !w.contains('.') && *w != "as")
                .unwrap_or("")
                .trim_matches(|c: char| c == '@' || c == '(' || c == ')' || c == '.')
                .to_owned();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// The provider + base URL configured for a project's git integration.
/// Whether the token-saver is enabled for a project (default on).
fn project_token_saver(p: &ProjectHandle) -> bool {
    std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .map_or(true, |c| c.workflow.token_saver)
}

async fn project_provider(app: &AppState, pid: &str) -> Option<(String, String)> {
    let p = app.project(pid).await?;
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    Some((cfg.git.provider, cfg.git.base_url))
}

/// Whether the project's git CLI is signed in, and as whom.
/// End-to-end git connection test: repo? remote? CLI authed? server
/// reachable? PUSH permitted? Each stage is a separate flag so the UI can say
/// exactly what's missing (imported-without-git, no remote, bad token, ...).
async fn git_test_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let wd = p.work_dir.clone();
    let is_repo = wd.join(".git").exists();
    let git = |args: &[&str]| {
        let mut c = tokio::process::Command::new("git");
        c.args(args)
            .current_dir(&wd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        c
    };
    let mut remote: Option<String> = None;
    let mut reachable = false;
    let mut push_ok = false;
    let mut detail = String::new();
    if is_repo {
        if let Ok(out) = git(&["remote", "get-url", "origin"]).output().await {
            if out.status.success() {
                let url = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if !url.is_empty() {
                    remote = Some(url);
                }
            }
        }
        if remote.is_some() {
            if let Ok(Ok(out)) = tokio::time::timeout(
                std::time::Duration::from_secs(12),
                git(&["ls-remote", "--heads", "origin"]).output(),
            )
            .await
            {
                reachable = out.status.success();
                if !reachable {
                    detail = String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .last()
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect();
                }
            } else {
                detail = "ls-remote timed out".to_owned();
            }
        }
        if reachable {
            // Dry-run push: proves PUSH permission without writing anything.
            if let Ok(Ok(out)) = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                git(&[
                    "push",
                    "--dry-run",
                    "origin",
                    "HEAD:refs/heads/coxagent-connection-test",
                ])
                .output(),
            )
            .await
            {
                push_ok = out.status.success();
                if !push_ok {
                    detail = String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .last()
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect();
                }
            } else {
                detail = "push --dry-run timed out".to_owned();
            }
        }
    }
    Json(serde_json::json!({
        "repo": is_repo,
        "remote": remote,
        "reachable": reachable,
        "push_ok": push_ok,
        "detail": detail,
    }))
    .into_response()
}

async fn git_auth_status_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some((provider, base)) = project_provider(&app, &pid).await else {
        return not_found();
    };
    let (bin, host_env) = git_cli(&provider);
    let host = base
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["auth", "status"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    if !host.is_empty() {
        cmd.env(host_env, &host);
    }
    let (present, authed, account) = match cmd.output().await {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (true, out.status.success(), parse_account(&combined))
        }
        Err(_) => (false, false, None),
    };
    Json(serde_json::json!({
        "tool": bin, "present": present, "authenticated": authed, "account": account,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct ConnectReq {
    token: String,
}

/// Sign the project's git CLI in with a user-supplied token, via stdin so the
/// token never appears in the process list; it is not stored or logged by
/// CoXAgent (the CLI keeps it in its own keyring). Admin-only.
async fn git_connect_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<ConnectReq>,
) -> axum::response::Response {
    let token = req.token.trim().to_owned();
    if token.is_empty() {
        return (StatusCode::BAD_REQUEST, "token required").into_response();
    }
    let Some((provider, base)) = project_provider(&app, &pid).await else {
        return not_found();
    };
    let (bin, host_env) = git_cli(&provider);
    let is_http = base.trim().starts_with("http://");
    let host = base
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    // Build login args: token on stdin, explicit hostname for self-hosted, and
    // the http protocol when the base URL isn't https (e.g. a local instance).
    let mut login_args: Vec<&str> = vec!["auth", "login"];
    if bin == "glab" {
        login_args.push("--stdin");
    } else {
        login_args.push("--with-token");
    }
    if !host.is_empty() {
        login_args.push("--hostname");
        login_args.push(&host);
    }
    if bin == "glab" && is_http {
        login_args.push("--api-protocol");
        login_args.push("http");
    }
    let host_env_val = if host.is_empty() { "" } else { host_env };
    let (ok, out) = {
        use tokio::io::AsyncWriteExt;
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(&login_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if !host_env_val.is_empty() {
            cmd.env(host_env_val, &host);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                if let Some(mut si) = child.stdin.take() {
                    let _ = si.write_all(token.as_bytes()).await;
                    let _ = si.shutdown().await;
                }
                match child.wait_with_output().await {
                    Ok(o) => (
                        o.status.success(),
                        String::from_utf8_lossy(&o.stderr).trim().to_owned(),
                    ),
                    Err(e) => (false, e.to_string()),
                }
            }
            Err(_) => (false, format!("{bin} not found")),
        }
    };
    if !ok {
        return (StatusCode::BAD_REQUEST, format!("sign-in failed: {out}")).into_response();
    }
    // Verify the token actually authenticates. `glab` stores a token without
    // validating it, so a successful login command is not enough — require the
    // status to resolve an account. If it doesn't, log the bad token back out so
    // we never leave broken credentials behind.
    let (status_ok, status_out) = run_cli_env(bin, &["auth", "status"], host_env_val, &host).await;
    let account = parse_account(&status_out);
    if !status_ok || account.is_none() {
        let host_arg = if host.is_empty() {
            if bin == "glab" {
                "gitlab.com"
            } else {
                "github.com"
            }
        } else {
            host.as_str()
        };
        let _ = run_cli(bin, &["auth", "logout", "--hostname", host_arg]).await;
        return (
            StatusCode::BAD_REQUEST,
            "token was rejected — check the token value and its scopes",
        )
            .into_response();
    }
    Json(serde_json::json!({ "ok": true, "account": account })).into_response()
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
    /// Clone this git URL and adopt it (brownfield import from remote).
    #[serde(default)]
    git_url: Option<String>,
    /// Confirmed project goal/context to seed (from AI-assisted drafting).
    #[serde(default)]
    goal: Option<String>,
    /// Space to file the new project under (super admin or that space's admin).
    #[serde(default)]
    space: Option<String>,
}

/// Onboard a new project from the dashboard (greenfield, or brownfield import
/// with `existing`, optionally seeded with a `goal`) via the injected factory.
async fn create_project(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateProjectReq>,
) -> axum::response::Response {
    // Resolve the target space up front — a bad/unauthorized space must fail
    // BEFORE the project is scaffolded, never leave a half-registered orphan.
    let space_id = req
        .space
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(sid) = space_id {
        let sup = is_super(&app, &headers).await;
        let me = resolve_username(&app, &headers).await;
        let doc = app.spaces.inner.lock().await;
        let Some(space) = doc.spaces.iter().find(|s| s.id == sid) else {
            return (StatusCode::BAD_REQUEST, format!("unknown space: {sid}")).into_response();
        };
        if !sup && !space.admins.iter().any(|a| a.eq_ignore_ascii_case(&me)) {
            return (StatusCode::FORBIDDEN, "not an admin of this space").into_response();
        }
    } else if !app.spaces.inner.lock().await.spaces.is_empty() {
        // Once spaces exist, every project must belong to one — enforced here,
        // not just in the UI, so API/service-account callers can't skip it.
        return (StatusCode::BAD_REQUEST, "space is required").into_response();
    }
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
    // Validate brownfield import path: must be under the hub's workspace root
    // or under /tmp (safe sandbox). Reject paths pointing to system directories.
    if let Some(ref existing) = req.existing {
        if !existing.trim().is_empty() {
            let p = std::path::Path::new(existing.trim());
            // Resolve to absolute canonical path to prevent symlink tricks.
            if let Ok(real) = p.canonicalize() {
                // Allow under /tmp or under $HOME (typical user repos).
                // Block system directories.
                let path_str = real.to_string_lossy();
                // Block if path equals a blocked directory, or if it starts with
                // a blocked directory plus '/', to catch `/private/etc/foo` etc.
                let blocked_prefixes = [
                    "/etc",
                    "/private/etc",
                    "/root",
                    "/var/run",
                    "/var/log",
                    "/usr/lib",
                    "/usr/sbin",
                    "/bin",
                    "/sbin",
                    "/dev",
                    "/proc",
                    "/sys",
                ];
                let blocked = blocked_prefixes.iter().any(|pfx| {
                    path_str == *pfx
                        || path_str.starts_with(pfx)
                            && path_str.as_bytes().get(pfx.len()).copied() == Some(b'/')
                });
                if blocked {
                    return (StatusCode::FORBIDDEN, "cannot import from this path").into_response();
                }
            }
        }
    }
    let handle = match factory(NewProjectReq {
        name,
        alias: req.alias,
        existing: req
            .existing
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from),
        git_url: req.git_url.filter(|s| !s.trim().is_empty()),
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
    if let Some(sid) = space_id {
        {
            let mut doc = app.spaces.inner.lock().await;
            if let Some(space) = doc.spaces.iter_mut().find(|s| s.id == sid) {
                if !space.projects.contains(&id) {
                    space.projects.push(id.clone());
                }
            }
        }
        app.spaces.save().await;
    }
    Json(serde_json::json!({ "ok": true, "id": id })).into_response()
}

#[derive(serde::Deserialize)]
struct RenameProjectReq {
    name: String,
}

/// Rename a project: persist the custom display name in its state and update the
/// in-memory handle so the change is live (no restart). Admin-only.
async fn rename_project_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<RenameProjectReq>,
) -> axum::response::Response {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return (StatusCode::BAD_REQUEST, "name must be 1–60 chars").into_response();
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    state.display_name = Some(name.to_owned());
    if let Err(e) = p.store.save(&state).await {
        return internal_error(&e.to_string());
    }
    // Reflect the new name in the live handle so list_projects returns it now.
    if let Some(h) = app.projects.write().await.get_mut(&pid) {
        name.clone_into(&mut h.name);
    }
    Json(serde_json::json!({ "ok": true, "name": name })).into_response()
}

/// Delete (deregister) a project: stop its runner, remove it from the hub, and
/// deregister it from the registry. The workspace files are left on disk.
async fn delete_project_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    // Deleting a project is destructive — restrict to admins and the lead tier
    // (Director/Manager/*Lead). Everyone else is forbidden. Super (hub-wide
    // owner) is included via `can_manage`.
    if let Some(auth) = &app.auth {
        let allowed = match resolve_principal(auth, &headers).await {
            Some(u) => u.role.can_manage(),
            None => false,
        };
        if !allowed {
            return (
                StatusCode::FORBIDDEN,
                "only an admin or manager may delete a project",
            )
                .into_response();
        }
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    p.runner.stop();
    app.projects.write().await.remove(&pid);
    app.order.write().await.retain(|id| id != &pid);
    // Clean up space references so no dangling project IDs remain.
    {
        let mut sp = app.spaces.inner.lock().await;
        for space in &mut sp.spaces {
            space.projects.retain(|p| p != &pid);
        }
        drop(sp);
        app.spaces.save().await;
    }
    if let Some(remover) = &app.remover {
        if let Err(e) = remover(pid.clone()).await {
            return internal_error(&e);
        }
    }
    // Clean up the project directory on disk. For imported projects this only
    // removes the CoXAgent workspace scaffolding (state/, coxagent.json, etc.)
    // — never the original imported codebase.
    if let Some(root) = p.config_path.parent() {
        let project_dir = root.to_path_buf();
        let codebase_linked = project_dir.join("codebase.lnk").exists();
        // Spawn cleanup in the background — errors are logged, never surfaced.
        tokio::spawn(async move {
            if codebase_linked {
                // Imported project: only delete CoXAgent scaffolding, not the code.
                let _ = std::fs::remove_file(project_dir.join("codebase.lnk"));
                if let Err(e) = std::fs::remove_dir_all(project_dir.join("state")) {
                    tracing::warn!("delete_project: cannot remove state dir: {e}");
                }
                let _ = std::fs::remove_file(project_dir.join("coxagent.json"));
                if let Ok(entries) = std::fs::read_dir(&project_dir) {
                    if entries.count() == 0 {
                        let _ = std::fs::remove_dir(&project_dir);
                    }
                }
            } else {
                // Greenfield: remove the entire project workspace.
                if let Err(e) = std::fs::remove_dir_all(&project_dir) {
                    tracing::warn!("delete_project: cannot remove project dir: {e}");
                }
            }
            // Also clean up the Docker compose project if it was deployed.
            let container_name = format!("cox-{pid}-codebase-app-1");
            if let Ok(out) = std::process::Command::new("docker")
                .args(["stop", &container_name])
                .output()
            {
                if !out.status.success() {
                    tracing::warn!("delete_project: docker stop {container_name} failed");
                }
            }
            let _ = std::process::Command::new("docker")
                .args(["rm", &container_name])
                .output();
        });
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
        escalation_level: 0,
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
    //
    // The SSE snapshot is broadcast to every viewer indiscriminately, so it may
    // only carry `#general` — private-channel messages are access-controlled and
    // reach members exclusively via the WebSocket / REST list, both of which
    // enforce membership.
    if let Some(chat) = v.get_mut("chat").and_then(serde_json::Value::as_array_mut) {
        chat.retain(|m| {
            m.get("channel").and_then(serde_json::Value::as_str)
                == Some(coxagent_application::GENERAL_CHANNEL)
        });
    }
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
                let mut v = serde_json::to_value(t).unwrap_or_default();
                // Cost-gate surface: the hold estimate (if any) and whether a
                // human already approved this ticket to run.
                if let Some(obj) = v.as_object_mut() {
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
                }
                Json(v).into_response()
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

/// Deterministic per-role performance + team quality stats for the Agents view.
async fn agent_evals_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match p.store.load().await {
        Ok(state) => Json(metrics::agent_evals(&state)).into_response(),
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

/// Token-saver effectiveness: aggregate the shim compression log (bytes before
/// vs after) into a headline "how much did we save" for the Cost view.
#[allow(clippy::cast_precision_loss)]
async fn token_saver_ep() -> axum::response::Response {
    let (mut samples, mut before, mut after) = (0u64, 0u64, 0u64);
    if let Ok(dir) = std::env::var("COXAGENT_SHIM_DIR") {
        if let Ok(text) = std::fs::read_to_string(std::path::Path::new(&dir).join("savings.log")) {
            for line in text.lines() {
                let mut it = line.split_whitespace();
                if let (Some(b), Some(a)) = (it.next(), it.next()) {
                    if let (Ok(b), Ok(a)) = (b.parse::<u64>(), a.parse::<u64>()) {
                        // A compressor row can never grow; such rows are torn
                        // concurrent writes — skip them instead of poisoning
                        // the totals.
                        if a <= b {
                            samples += 1;
                            before += b;
                            after += a;
                        }
                    }
                }
            }
        }
    }
    let saved = before.saturating_sub(after);
    let pct = if before > 0 {
        (saved as f64 / before as f64) * 100.0
    } else {
        0.0
    };
    Json(serde_json::json!({
        "samples": samples, "before": before, "after": after,
        "saved": saved, "pct": (pct * 10.0).round() / 10.0,
    }))
    .into_response()
}

/// The shared worker registry: every team (`account@host`) currently online for
/// this project, across all machines. Powers the dashboard's cross-machine view.
async fn workers_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let workers = p.store.workers().await.unwrap_or_default();
    Json(workers).into_response()
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
        escalation_level: 0,
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
async fn ticket_refine(
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

/// Un-park a ticket: clear its fail-attempt counter and journal so agents
/// pick it up again — the human's "this deserves another shot" button.
async fn unpark_ticket(
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
async fn approve_cost(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    if state.cost_holds.remove(&id).is_none()
        && !state.tickets.iter().any(|t| t.id().as_str() == id)
    {
        return (axum::http::StatusCode::NOT_FOUND, "no such ticket").into_response();
    }
    state.cost_approved.insert(id.clone());
    state.log_activity("USER", "approved cost", Some(id));
    match p.store.save(&state).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
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

#[derive(serde::Deserialize)]
struct EditReq {
    title: String,
    #[serde(default)]
    description: String,
}

/// Edit a ticket's title + description (scope owner action).
async fn edit_ticket(
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
    #[serde(default)]
    attachments: Vec<coxagent_application::Attachment>,
}

#[derive(serde::Deserialize)]
struct CommentReactReq {
    emoji: String,
}

/// Post a comment (as the user) to a ticket thread or the team channel.
async fn post_comment(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PostCommentReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let body = req.body.trim();
    if body.is_empty() && req.attachments.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "empty comment").into_response();
    }
    // Attribute the comment to the signed-in account (so Scrum/discussion shows
    // real names, not a generic "USER"); falls back to "USER" in open mode.
    let author = resolve_username(&app, &headers).await;
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    state.post_comment_att(&author, body, req.ticket.clone(), req.attachments.clone());
    if let Err(e) = p.store.save(&state).await {
        return internal_error(&e.to_string());
    }
    // If the user attached something an agent can read, let the SA agent read it
    // and respond — answering if a question was asked, otherwise reading it
    // proactively and asking back. Runs in the background so the post is instant.
    maybe_analyze_attachments(&app, &p, &author, body, req.ticket, &req.attachments);
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Toggle the caller's emoji reaction on a ticket/discussion comment.
async fn comment_react_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CommentReactReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let user = resolve_username(&app, &headers).await;
    let emoji = req.emoji.chars().take(8).collect::<String>();
    if emoji.is_empty() {
        return (StatusCode::BAD_REQUEST, "emoji required").into_response();
    }
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(updated) = state.react_comment(&id, &user, &emoji) else {
        return not_found();
    };
    if let Err(e) = p.store.save(&state).await {
        return internal_error(&e.to_string());
    }
    Json(updated).into_response()
}

/// Spawn a background SA turn that reads any readable attachments on a freshly
/// posted comment and replies. No-op when there are no readable attachments.
fn maybe_analyze_attachments(
    app: &AppState,
    p: &ProjectHandle,
    author: &str,
    body: &str,
    ticket: Option<String>,
    attachments: &[coxagent_application::Attachment],
) {
    use coxagent_application::use_cases::{AnalyzeAttachmentUseCase, ReadableAttachment};
    // Storage keys for each attachment (the CLI reads local files, so we fetch
    // the bytes from the blob store into a temp dir — works for disk and S3).
    let items: Vec<(String, String, String)> = attachments
        .iter()
        .filter_map(|a| {
            let file = a.url.rsplit('/').next()?;
            Some((
                a.name.clone(),
                a.mime.clone(),
                format!("proj/{}/{file}", p.id),
            ))
        })
        .collect();
    if items.is_empty() {
        return;
    }
    let storage = Arc::clone(&app.storage);
    let store = Arc::clone(&p.store);
    let engine = Arc::clone(&p.engine);
    let work_dir = p.work_dir.clone();
    let (author, body) = (author.to_owned(), body.to_owned());
    tokio::spawn(async move {
        let tmp = std::env::temp_dir().join(format!("cox-att-{}", mint_media_token()));
        let _ = std::fs::create_dir_all(&tmp);
        let mut readable: Vec<ReadableAttachment> = Vec::new();
        for (name, mime, key) in items {
            let Ok(bytes) = storage.get(&key).await else {
                continue;
            };
            let fname = key.rsplit('/').next().unwrap_or("file");
            let path = tmp.join(fname);
            if std::fs::write(&path, &bytes).is_ok() {
                readable.push(ReadableAttachment { name, mime, path });
            }
        }
        if !readable.is_empty() {
            let uc = AnalyzeAttachmentUseCase::new(store, engine, work_dir);
            if let Err(e) = uc.execute(&author, &body, ticket, &readable).await {
                tracing::warn!("attachment analysis failed: {e}");
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    });
}

#[derive(serde::Deserialize)]
struct ChatListQuery {
    /// Which channel's history to return; defaults to `#general`.
    channel: Option<String>,
}

/// List a channel's team-chat messages (oldest first). Filters to `?channel=`
/// (default `#general`); returns empty for a channel the caller can't view.
async fn chat_list_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ChatListQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let channel = q
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    let user = resolve_username(&app, &headers).await;
    let Ok(state) = p.store.load().await else {
        return Json(Vec::<coxagent_application::ChatMsg>::new()).into_response();
    };
    match state.channel(&channel) {
        Some(c) if c.can_view(&user) => {}
        _ => return Json(Vec::<coxagent_application::ChatMsg>::new()).into_response(),
    }
    let chat: Vec<_> = state
        .chat
        .into_iter()
        .filter(|m| m.channel == channel)
        .collect();
    Json(chat).into_response()
}

/// List the channels the signed-in user can see (`#general` first).
async fn channels_list_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let user = resolve_username(&app, &headers).await;
    let channels = p
        .store
        .load()
        .await
        .map(|s| s.channels_for(&user))
        .unwrap_or_default();
    Json(channels).into_response()
}

#[derive(serde::Deserialize)]
struct CreateChannelReq {
    name: String,
    #[serde(default)]
    kind: Option<String>,
}

/// Create a private channel owned by the signed-in user.
async fn channel_create_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateChannelReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let user = resolve_username(&app, &headers).await;
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    match state.create_channel(&req.name, &user) {
        Ok(ch) => match p.store.save(&state).await {
            Ok(()) => (StatusCode::CREATED, Json(ch)).into_response(),
            Err(e) => internal_error(&e.to_string()),
        },
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct ChannelMemberReq {
    /// Username to invite or delegate to.
    user: String,
    /// When true, grant invite permission (owner only), not just membership.
    #[serde(default)]
    delegate: bool,
}

/// Invite a user to a channel, or (with `delegate`) grant them invite rights.
async fn channel_invite_ep(
    State(app): State<AppState>,
    Path((pid, cid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChannelMemberReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let actor = resolve_username(&app, &headers).await;
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let result = if req.delegate {
        state.delegate_invite(&cid, &actor, &req.user)
    } else {
        state.invite_to_channel(&cid, &actor, &req.user)
    };
    match result {
        Ok(()) => match p.store.save(&state).await {
            Ok(()) => Json(state.channel(&cid)).into_response(),
            Err(e) => internal_error(&e.to_string()),
        },
        Err(msg) => (StatusCode::FORBIDDEN, msg).into_response(),
    }
}

/// Resolve the signed-in username, or `"user"` when auth is disabled.
async fn resolve_username(app: &AppState, headers: &axum::http::HeaderMap) -> String {
    match &app.auth {
        Some(auth) => resolve_principal(auth, headers)
            .await
            .map_or_else(|| "user".to_owned(), |u| u.username),
        None => "user".to_owned(),
    }
}

#[derive(serde::Deserialize)]
struct PostChatReq {
    body: String,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    attachments: Vec<coxagent_application::Attachment>,
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
    if body.is_empty() && req.attachments.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "empty message").into_response();
    }
    if body.chars().count() > 2000 {
        return (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "message too long",
        )
            .into_response();
    }
    let user = resolve_username(&app, &headers).await;
    let channel = req
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    if deliver_chat(&app, &p, &user, body, &channel, req.attachments).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        // Either persistence failed or the user isn't a member of the channel.
        (StatusCode::FORBIDDEN, "cannot post to this channel").into_response()
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

/// List the project's documentation pages.
async fn docs_list_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let docs = app.doc_list(&pid, &p).await;
    Json(docs).into_response()
}

#[derive(serde::Deserialize)]
struct DocUpsertReq {
    #[serde(default)]
    folder: String,
    title: String,
    #[serde(default)]
    body: String,
}

/// Mint an id for a brand-new page (used only when the client sends none).
fn mint_doc_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("doc-{nanos}")
}

/// Colour bucket from a folder path's top segment.
fn doc_category(folder: &str) -> &'static str {
    match folder
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "technical" | "architecture" | "engineering" => "technical",
        "flows" | "design" => "flows",
        "testing" | "qa" | "test" | "tests" => "qa",
        "operations" | "ops" | "release notes" | "releases" => "ops",
        _ => "product",
    }
}

/// Create or update a documentation page.
async fn doc_upsert_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<DocUpsertReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    if req.title.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "title required").into_response();
    }
    let author = resolve_username(&app, &headers).await;
    // A client-minted "new-<ts>" placeholder id means "create"; treat as empty.
    let id = if id.starts_with("new-") {
        ""
    } else {
        id.as_str()
    };
    match app
        .doc_upsert(
            &pid,
            &p,
            id,
            req.folder.trim(),
            req.title.trim(),
            &req.body,
            &author,
        )
        .await
    {
        Ok(page) => Json(page).into_response(),
        Err(e) => internal_error(&e),
    }
}

#[derive(serde::Deserialize)]
struct DocEditReq {
    instruction: String,
}

/// Ask the DOCS agent to revise one page per a human instruction.
async fn doc_ai_edit_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    Json(req): Json<DocEditReq>,
) -> axum::response::Response {
    use coxagent_application::use_cases::GenerateDocsUseCase;
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(page) = app.doc_get(&pid, &p, &id).await else {
        return not_found();
    };
    if req.instruction.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "instruction required").into_response();
    }
    let uc = GenerateDocsUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
    );
    match uc
        .revise(
            &page.folder,
            &page.title,
            &page.body,
            req.instruction.trim(),
        )
        .await
    {
        Ok(body) => match app
            .doc_upsert(&pid, &p, &id, &page.folder, &page.title, &body, "DOCS")
            .await
        {
            Ok(saved) => Json(saved).into_response(),
            Err(e) => internal_error(&e),
        },
        Err(e) => internal_error(&format!("doc edit failed: {e}")),
    }
}

/// Delete a documentation page.
async fn doc_delete_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match app.doc_delete(&pid, &p, &id).await {
        Ok(removed) => Json(serde_json::json!({ "ok": removed })).into_response(),
        Err(e) => internal_error(&e),
    }
}

#[derive(serde::Deserialize)]
struct FolderReq {
    path: String,
}

/// The explicit Wiki folder paths (empty folders included).
async fn doc_folders_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let folders = p
        .store
        .load()
        .await
        .map(|s| s.doc_folders)
        .unwrap_or_default();
    Json(folders).into_response()
}

/// Create a (possibly nested) Wiki folder.
async fn doc_folder_add_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<FolderReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(mut s) = p.store.load().await else {
        return internal_error("load failed");
    };
    s.add_doc_folder(&req.path);
    match p.store.save(&s).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Delete a Wiki folder and everything under it (subfolders + pages).
async fn doc_folder_del_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<FolderReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let path = req.path.trim().trim_matches('/').to_owned();
    let prefix = format!("{path}/");
    // Delete pages under the folder from whichever store holds them.
    for page in app.doc_list(&pid, &p).await {
        if page.folder == path || page.folder.starts_with(&prefix) {
            let _ = app.doc_delete(&pid, &p, &page.id).await;
        }
    }
    let Ok(mut s) = p.store.load().await else {
        return internal_error("load failed");
    };
    s.remove_doc_folder(&path);
    match p.store.save(&s).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Move a page into another folder (works across the state/Mongo stores).
async fn doc_move_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    Json(req): Json<FolderReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(page) = app.doc_get(&pid, &p, &id).await else {
        return (StatusCode::NOT_FOUND, "no such page").into_response();
    };
    match app
        .doc_upsert(&pid, &p, &id, &req.path, &page.title, &page.body, "USER")
        .await
    {
        Ok(_) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e),
    }
}

/// Generate/refresh the documentation with the DOCS agent.
async fn docs_generate_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    use coxagent_application::use_cases::GenerateDocsUseCase;
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let md = tokio::fs::read_to_string(&p.context_path)
        .await
        .unwrap_or_default();
    let uc = GenerateDocsUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
    )
    .with_token_saver(project_token_saver(&p));
    let pages = match uc.execute(&md).await {
        Ok(pages) => pages,
        Err(e) => return internal_error(&format!("docs generation failed: {e}")),
    };
    let mut written = 0usize;
    for page in &pages {
        if app
            .doc_upsert(
                &pid,
                &p,
                &page.id,
                &page.folder,
                &page.title,
                &page.body,
                &page.updated_by,
            )
            .await
            .is_ok()
        {
            written += 1;
        }
    }
    Json(serde_json::json!({ "ok": true, "pages": written })).into_response()
}

/// Live collaborative-edit WebSocket for one documentation page. Same-origin
/// guarded + authenticated. Peers in the room see each other's edits and a live
/// presence roster; every save is persisted through the active doc store.
async fn docs_ws_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Some(resp) = role_guard(true) {
        return resp;
    }
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    let user = match &app.auth {
        Some(auth) => match resolve_principal(auth, &headers).await {
            Some(u) => u.username,
            None => return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response(),
        },
        None => "user".to_owned(),
    };
    if app.project(&pid).await.is_none() {
        return not_found();
    }
    let ws = ws.max_message_size(256 * 1024);
    ws.on_upgrade(move |socket| docs_socket(socket, app, pid, id, user))
}

/// Drive one live-edit socket: broadcast presence on join/leave, persist + fan
/// out each save to the room.
async fn docs_socket(mut socket: WebSocket, app: AppState, pid: String, id: String, user: String) {
    let room = format!("{pid}/{id}");
    let tx = app.docs_room(&room).await;
    let mut rx = tx.subscribe();
    // Announce arrival and push the fresh roster to everyone (incl. this socket).
    let roster = app.docs_presence(&room, &user, true);
    let _ = tx.send(presence_json(&roster));
    loop {
        tokio::select! {
            bcast = rx.recv() => {
                match bcast {
                    Ok(json) => {
                        if socket.send(Message::Text(json)).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                match v.get("op").and_then(|o| o.as_str()) {
                    Some("save") => {
                        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or_default();
                        let folder = v.get("folder").and_then(|x| x.as_str()).unwrap_or_default();
                        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or_default();
                        let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or_default();
                        if title.trim().is_empty() { continue; }
                        let Some(p) = app.project(&pid).await else { continue };
                        if app.doc_upsert(&pid, &p, &id, folder, title, body, &user).await.is_ok() {
                            let out = serde_json::json!({
                                "op": "doc", "id": id, "title": title, "folder": folder,
                                "body": body, "by": user, "origin": origin,
                            });
                            let _ = tx.send(out.to_string());
                        }
                    }
                    // A pure "typing" ping keeps presence lively without a save.
                    Some("ping") => {
                        let roster = {
                            let mut m = match app.docs_editors.lock() { Ok(m) => m, Err(p) => p.into_inner() };
                            m.entry(room.clone()).or_default().keys().cloned().collect::<Vec<_>>()
                        };
                        let _ = tx.send(presence_json(&roster));
                    }
                    _ => {}
                }
            }
        }
    }
    let roster = app.docs_presence(&room, &user, false);
    let _ = tx.send(presence_json(&roster));
}

/// Serialise a presence roster broadcast.
fn presence_json(editors: &[String]) -> String {
    serde_json::json!({ "op": "presence", "editors": editors }).to_string()
}

/// A compact overview of a loaded code graph (never the full node list).
fn codegraph_summary(g: &coxagent_application::codegraph::CodeGraph) -> serde_json::Value {
    let mut top: Vec<&coxagent_application::codegraph::FileNode> = g.files.iter().collect();
    top.sort_by(|a, b| b.symbols.cmp(&a.symbols).then(b.loc.cmp(&a.loc)));
    let top_files: Vec<serde_json::Value> = top
        .iter()
        .take(40)
        .map(|f| {
            serde_json::json!({
                "path": f.path, "lang": f.lang, "loc": f.loc,
                "symbols": f.symbols, "imports": f.imports.len(),
            })
        })
        .collect();
    serde_json::json!({
        "built": true,
        "built_at": g.built_at,
        "files": g.files.len(),
        "symbols": g.symbols.len(),
        "edges": g.edges.len(),
        "languages": g.languages,
        "top_files": top_files,
    })
}

#[derive(serde::Deserialize)]
struct CodeGraphQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    map: Option<u8>,
}

/// Read the code graph: overview, `?q=` symbol search, or `?map=1` repo map.
async fn codegraph_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(query): axum::extract::Query<CodeGraphQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(g) = coxagent_application::codegraph::CodeGraph::load(&p.work_dir) else {
        return Json(serde_json::json!({ "built": false })).into_response();
    };
    if let Some(q) = query.q.filter(|q| !q.trim().is_empty()) {
        let results: Vec<_> = g.relevance_search(&q, 60);
        return Json(serde_json::json!({ "built": true, "results": results })).into_response();
    }
    if query.map.unwrap_or(0) == 1 {
        return Json(serde_json::json!({ "built": true, "map": g.repo_map(20_000) }))
            .into_response();
    }
    Json(codegraph_summary(&g)).into_response()
}

/// The internal dependency graph (file → file import edges) for visualisation.
/// Bounded to the most-connected files so the picture stays legible.
async fn codegraph_deps_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(g) = coxagent_application::codegraph::CodeGraph::load(&p.work_dir) else {
        return Json(serde_json::json!({ "built": false })).into_response();
    };
    let edges = g.resolved_edges();
    // Degree per file (in + out) to pick the interesting nodes.
    let mut degree: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (a, b) in &edges {
        *degree.entry(a.as_str()).or_insert(0) += 1;
        *degree.entry(b.as_str()).or_insert(0) += 1;
    }
    let mut ranked: Vec<(&str, usize)> = degree.into_iter().collect();
    ranked.sort_by_key(|&(_, d)| std::cmp::Reverse(d));
    let keep: std::collections::HashSet<&str> = ranked.iter().take(60).map(|(f, _)| *f).collect();
    let sym_of: std::collections::HashMap<&str, usize> = g
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.symbols))
        .collect();
    let lang_of: std::collections::HashMap<&str, &str> = g
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.lang.as_str()))
        .collect();
    let nodes: Vec<serde_json::Value> = keep
        .iter()
        .map(|f| {
            serde_json::json!({
                "id": f,
                "lang": lang_of.get(f).copied().unwrap_or(""),
                "symbols": sym_of.get(f).copied().unwrap_or(0),
            })
        })
        .collect();
    let links: Vec<serde_json::Value> = edges
        .iter()
        .filter(|(a, b)| keep.contains(a.as_str()) && keep.contains(b.as_str()))
        .map(|(a, b)| serde_json::json!({ "source": a, "target": b }))
        .collect();
    Json(serde_json::json!({
        "built": true, "nodes": nodes, "links": links, "total_files": g.files.len(),
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct RefsQuery {
    name: String,
}

/// Impact analysis: every whole-word usage of a symbol across the working tree.
async fn codegraph_refs_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(query): axum::extract::Query<RefsQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let name = query.name.trim().to_owned();
    if name.len() < 2 {
        return (StatusCode::BAD_REQUEST, "name too short").into_response();
    }
    let work_dir = p.work_dir.clone();
    let scan_name = name.clone();
    let refs = tokio::task::spawn_blocking(move || {
        coxagent_application::codegraph::references(&work_dir, &scan_name, 200)
    })
    .await
    .unwrap_or_default();
    let defs = refs.iter().filter(|r| r.is_def).count();
    // Call graph (from the persisted index): who calls this fn, and what it calls.
    let (inbound, outbound) = coxagent_application::codegraph::CodeGraph::load(&p.work_dir)
        .map(|g| (g.callers(&name), g.callees(&name)))
        .unwrap_or_default();
    let cg = |v: Vec<(String, String, usize)>| -> Vec<serde_json::Value> {
        v.into_iter()
            .take(100)
            .map(|(label, file, line)| serde_json::json!({ "label": label, "file": file, "line": line }))
            .collect()
    };
    let inbound_n = inbound.len();
    Json(serde_json::json!({
        "name": query.name.trim(),
        "total": refs.len(),
        "defs": defs,
        "uses": refs.len() - defs,
        "refs": refs,
        "callers": cg(inbound),
        "callees": cg(outbound),
        "caller_count": inbound_n,
    }))
    .into_response()
}

/// (Re)build the code graph for a project by indexing its working tree.
async fn codegraph_build_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let work_dir = p.work_dir.clone();
    // Indexing is pure file I/O + string work — run it off the async runtime.
    let result = tokio::task::spawn_blocking(move || {
        let g = coxagent_application::codegraph::CodeGraph::index(&work_dir);
        g.save(&work_dir)?;
        // Also drop a readable repo map the CLI agents will find naturally.
        let _ = std::fs::write(
            work_dir.join(".coxagent").join("REPO_MAP.md"),
            g.repo_map(40_000),
        );
        std::io::Result::Ok(g)
    })
    .await;
    match result {
        Ok(Ok(g)) => Json(codegraph_summary(&g)).into_response(),
        Ok(Err(e)) => internal_error(&format!("codegraph save failed: {e}")),
        Err(e) => internal_error(&format!("codegraph build failed: {e}")),
    }
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

/// List open pull/merge requests for a project's repository.
async fn list_prs_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(forge) = &p.forge else {
        return Json(serde_json::json!({ "configured": false, "prs": [] })).into_response();
    };
    // The SA's stored review verdict per PR, so the UI can show the suggestion.
    let reviews = p.store.load().await.map(|s| s.reviews).unwrap_or_default();
    let auto_merge = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .is_some_and(|c| c.git.auto_merge);
    match forge.list_open_prs().await {
        Ok(prs) => {
            let enriched: Vec<serde_json::Value> = prs
                .iter()
                .map(|pr| {
                    let mut v = serde_json::to_value(pr).unwrap_or_default();
                    if let Some(r) = reviews.iter().find(|r| r.number == pr.number) {
                        v["review"] = serde_json::json!({
                            "decision": r.decision, "summary": r.summary, "at": r.at,
                        });
                    }
                    v
                })
                .collect();
            Json(serde_json::json!({
                "configured": true, "auto_merge": auto_merge, "prs": enriched
            }))
            .into_response()
        }
        Err(e) => {
            Json(serde_json::json!({ "configured": true, "error": e.to_string(), "prs": [] }))
                .into_response()
        }
    }
}

/// The unified diff of one PR (for the in-app review view).
async fn pr_diff_ep(
    State(app): State<AppState>,
    Path((pid, num)): Path<(String, u64)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(forge) = &p.forge else {
        return (StatusCode::NOT_IMPLEMENTED, "forge not configured").into_response();
    };
    match forge.pr_diff(num).await {
        Ok(diff) => Json(serde_json::json!({ "diff": diff })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct PrActionReq {
    #[serde(default)]
    comment: String,
}

/// A review action on a PR (`merge` / `request-changes` / `close`). Requires a
/// reviewer or admin (enforced by [`auth_mw`]).
async fn pr_action_ep(
    State(app): State<AppState>,
    Path((pid, num, action)): Path<(String, u64, String)>,
    body: Option<Json<PrActionReq>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(forge) = &p.forge else {
        return (StatusCode::NOT_IMPLEMENTED, "forge not configured").into_response();
    };
    let comment = body.map(|b| b.0.comment).unwrap_or_default();
    // Preview actions deploy code; they answer with their own payload.
    if action == "preview" || action == "preview-stop" {
        return pr_preview(&p, forge, num, action == "preview").await;
    }
    // Force-merge runs in the background (conflict fix can take minutes).
    // Per-PR in-flight guard: two users clicking Force at once would run two
    // engines in the SAME work_dir, corrupting each other's resolution.
    if action == "force-merge" {
        // Execution-plane routing: with a live runner registered, the job is
        // queued for IT to execute (the control plane never runs engines when
        // it doesn't have to); the runner's 15s poll picks it up. Only when no
        // runner is alive does the hub fall back to executing inline.
        let live_runner = p.store.workers().await.is_ok_and(|w| !w.is_empty());
        if live_runner {
            let queued =
                coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
                    if s.jobs.iter().any(|j| {
                        j.kind == "force_merge" && j.args.get("pr") == Some(&serde_json::json!(num))
                    }) {
                        return Ok(()); // already queued — idempotent
                    }
                    s.jobs.push(coxagent_application::state::PendingJob {
                        id: coxagent_application::state::mint_id(),
                        kind: "force_merge".to_owned(),
                        args: serde_json::json!({ "pr": num }),
                        queued_at: coxagent_application::state::now_rfc3339(),
                        queued_by: "web".to_owned(),
                    });
                    Ok(())
                })
                .await;
            if queued.is_ok() {
                return Json(serde_json::json!({ "ok": true, "queued": true })).into_response();
            }
        }
        let key = (pid.clone(), num);
        {
            let mut inflight = force_inflight().lock().await;
            if !inflight.insert(key.clone()) {
                return (
                    StatusCode::CONFLICT,
                    "force-merge for this PR is already running",
                )
                    .into_response();
            }
        }
        let handle = p.clone();
        tokio::spawn(async move {
            force_merge(handle, num).await;
            force_inflight().lock().await.remove(&key);
        });
        return Json(serde_json::json!({ "ok": true, "started": true })).into_response();
    }
    let result = match action.as_str() {
        "merge" => forge.merge_pr(num).await,
        "request-changes" => {
            let c = if comment.trim().is_empty() {
                "Changes requested via CoXAgent review.".to_owned()
            } else {
                comment
            };
            forge.request_changes(num, &c).await
        }
        "close" => forge.close_pr(num).await,
        _ => return (StatusCode::BAD_REQUEST, "unknown action").into_response(),
    };
    match result {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Force-merge one PR on the human's order: if it's already green, merge now;
/// if it's blocked (conflicts), a DEV agent resolves them IMMEDIATELY (not next
/// cycle), pushes, and then the merge lands. Progress is narrated in `#agents`.
/// (project id, PR number) pairs with a force-merge currently running — the
/// hub-wide guard against concurrent resolutions in one work_dir.
fn force_inflight() -> &'static tokio::sync::Mutex<std::collections::HashSet<(String, u64)>> {
    static SET: std::sync::OnceLock<tokio::sync::Mutex<std::collections::HashSet<(String, u64)>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| tokio::sync::Mutex::new(std::collections::HashSet::new()))
}

async fn force_merge(p: ProjectHandle, num: u64) {
    let Some(forge) = p.forge.clone() else { return };
    let say = |msg: String| {
        let store = Arc::clone(&p.store);
        async move {
            let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
                s.post_chat_in(
                    "SA",
                    &msg,
                    coxagent_application::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
                Ok(())
            })
            .await;
        }
    };
    let find = || async {
        forge
            .list_open_prs()
            .await
            .ok()
            .and_then(|prs| prs.into_iter().find(|x| x.number == num))
    };
    let Some(pr) = find().await else {
        say(format!("⚡ Force-merge #{num}: PR không còn mở — bỏ qua.")).await;
        return;
    };
    // Blocked? Fix it right now with a DEV engine pass.
    if !pr.mergeable {
        say(format!(
            "⚡ Force-merge #{num}: đang gỡ conflict trên `{}` ngay bây giờ…",
            pr.head
        ))
        .await;
        let request = coxagent_application::ports::outbound::AgentRequest {
            role: coxagent_domain::Role::DevBug,
            system_prompt: coxagent_application::prompts::system_prompt(
                coxagent_application::prompts::DEV,
            ),
            task_prompt: format!(
                "URGENT: a human ordered PR #{num} (branch `{h}`) force-merged. It has merge \
                 conflicts with `{b}`.\n\
                 1. `git fetch origin && git checkout {h} && git pull origin {h}`\n\
                 2. `git merge origin/{b}` and resolve EVERY conflict, preserving both this \
                 branch's fix and what already landed on {b}.\n\
                 3. Run the build/tests to make sure nothing broke.\n\
                 4. `git add -A && git commit -m \"fix: resolve conflicts for #{num}\"` then \
                 `git push origin {h}`.",
                h = pr.head,
                b = pr.base,
            ),
            work_dir: p.work_dir.clone(),
            timeout: std::time::Duration::from_secs(1800),
            escalation_level: 0,
        };
        match p.engine.run(request).await {
            Ok(o) if o.succeeded() => {}
            _ => {
                say(format!(
                    "⚡ Force-merge #{num}: gỡ conflict THẤT BẠI — cần bạn xử lý tay: {}",
                    pr.url
                ))
                .await;
                return;
            }
        }
        // Give the forge a moment to recompute mergeability.
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
    // Merge (retry once after a short wait — mergeability can lag a push).
    for attempt in 0..2u8 {
        match forge.merge_pr(num).await {
            Ok(()) => {
                say(format!("⚡ Force-merge #{num}: ĐÃ MERGE ✓")).await;
                return;
            }
            Err(e) if attempt == 0 => {
                tracing::warn!("force-merge #{num} first attempt: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            }
            Err(e) => {
                say(format!(
                    "⚡ Force-merge #{num}: merge bị từ chối ({e}) — xem PR: {}",
                    pr.url
                ))
                .await;
            }
        }
    }
}

/// Run one git command in `dir`, surfacing stderr on failure.
async fn git_pv(dir: &std::path::Path, args: &[&str]) -> Result<(), String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// The app port `coxagent.json` publishes, or `None` when the project doesn't
/// declare one.
///
/// A config that can't be read or parsed at all is "no port published" — the
/// preview still runs, it just has nothing to link to or probe. A
/// `deploy.host_port` that IS declared but isn't a TCP port is a different
/// case, and [`probe_port`] refuses it rather than folding it into `None`.
///
/// # Errors
/// The reason to report when the configured port isn't a TCP port.
fn published_host_port(config_path: &std::path::Path) -> Result<Option<u16>, String> {
    let Some(config) = std::fs::read_to_string(config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    else {
        return Ok(None);
    };
    probe_port(&config["deploy"]["host_port"])
}

/// Narrow the raw `deploy.host_port` JSON value to the port the COX-B009
/// health gate probes.
///
/// `config.deploy.host_port` is an `Option<u16>` everywhere else, so any value
/// this rejects is one the app's own typed config would refuse to load — a
/// hand-edited or corrupted file, or a future writer that doesn't respect the
/// `u16` invariant. Refusing it is the only safe reading: truncating a wider
/// number would silently probe a DIFFERENT port, and folding a malformed value
/// into "unset" would skip the gate entirely — both hand back the unverified
/// "LIVE" the gate exists to prevent. Only an absent or `null` port is
/// `Ok(None)`, the one case where there genuinely is nothing to probe.
///
/// # Errors
/// The reason to report when the configured port isn't a TCP port.
fn probe_port(host_port: &serde_json::Value) -> Result<Option<u16>, String> {
    // A missing key indexes to `Null`, same as an explicit `null`: unset.
    if host_port.is_null() {
        return Ok(None);
    }
    host_port
        .as_u64()
        .and_then(|pt| u16::try_from(pt).ok())
        .map(Some)
        .ok_or_else(|| {
            format!(
                "deploy.host_port {host_port} is not a valid TCP port — refusing to deploy a \
                 preview whose health can't be verified"
            )
        })
}

/// Whether one preview `deploy` call may be reported as a success: `None` if
/// it may, `Some(reason)` if it may not.
///
/// Both preview paths — starting a preview and restoring the main build —
/// decide through this one function, so neither can report a deploy as good on
/// the compose exit code alone. A `docker compose up` exit 0 only proves the
/// containers started; the mandatory health gate (COX-B004/COX-B009) is what
/// proves the app inside actually bound its port.
async fn preview_deploy_failure(
    deploy: &Arc<dyn coxagent_application::ports::outbound::DeployPort>,
    report: &coxagent_application::ports::outbound::DeployReport,
    probe_port: Option<u16>,
) -> Option<String> {
    if !report.success {
        return Some(report.summary.clone());
    }
    if coxagent_application::ports::outbound::verify_deploy_health(deploy, probe_port).await {
        return None;
    }
    Some(format!(
        "{} (containers started but the app never bound its port — health check failed)",
        report.summary
    ))
}

/// Deploy a PR's branch so the human can SEE the change running before
/// approving (start=true), or tear the preview down and restore main
/// (start=false). The preview runs on the project's app port — one app at a
/// time, honestly labeled — via a git worktree under `<workspace>/.preview/`.
async fn pr_preview(
    p: &ProjectHandle,
    forge: &Arc<dyn coxagent_application::ports::outbound::ForgePort>,
    num: u64,
    start: bool,
) -> axum::response::Response {
    let Some(deploy) = &p.deploy else {
        return (StatusCode::NOT_IMPLEMENTED, "deploy not configured").into_response();
    };
    let root = p
        .config_path
        .parent()
        .unwrap_or(&p.config_path)
        .to_path_buf();
    let prev_dir = root.join(".preview").join(num.to_string());
    // The project's published app port: both the "open it" link and the health
    // gate's probe target, resolved once so the link can never name a port the
    // gate didn't actually probe. Resolved before either path deploys, so a
    // config that can't be verified is refused rather than reported "LIVE".
    let port = match published_host_port(&p.config_path) {
        Ok(pt) => pt,
        Err(why) => return internal_error(&why),
    };
    let chat = |msg: String| {
        let store = Arc::clone(&p.store);
        async move {
            let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
                s.post_chat_in(
                    "COX",
                    &msg,
                    coxagent_application::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
                Ok(())
            })
            .await;
        }
    };
    if start {
        // Resolve the PR's head branch and materialise it in a preview worktree.
        let head = match forge.list_open_prs().await {
            Ok(prs) => match prs.into_iter().find(|x| x.number == num) {
                Some(x) => x.head,
                None => return (StatusCode::NOT_FOUND, "PR not open").into_response(),
            },
            Err(e) => return internal_error(&e.to_string()),
        };
        let refspec = format!("origin/{head}");
        let step = if prev_dir.exists() {
            git_pv(&p.work_dir, &["fetch", "origin", &head])
                .await
                .and(git_pv(&prev_dir, &["reset", "--hard", &refspec]).await)
        } else {
            let _ = std::fs::create_dir_all(prev_dir.parent().unwrap_or(&root));
            git_pv(&p.work_dir, &["fetch", "origin", &head]).await.and(
                git_pv(
                    &p.work_dir,
                    &[
                        "worktree",
                        "add",
                        "--force",
                        &prev_dir.to_string_lossy(),
                        &refspec,
                    ],
                )
                .await,
            )
        };
        if let Err(e) = step {
            return internal_error(&format!("preview checkout: {e}"));
        }
        // Swap: stop the current app, run the PR branch on the app port.
        let _ = deploy.down(&p.work_dir).await;
        match deploy.deploy(&prev_dir).await {
            Ok(r) => {
                if let Some(why) = preview_deploy_failure(deploy, &r, port).await {
                    internal_error(&format!("preview deploy failed: {why}"))
                } else {
                    let url = port.map(|pt| format!("http://localhost:{pt}"));
                    chat(format!(
                        "👁 Preview of PR #{num} is LIVE{} — the main build is paused; restore it from the Review tab when done.",
                        url.as_deref().map(|u| format!(" at {u}")).unwrap_or_default()
                    ))
                    .await;
                    Json(serde_json::json!({ "ok": true, "url": url, "summary": r.summary }))
                        .into_response()
                }
            }
            Err(e) => internal_error(&e.to_string()),
        }
    } else {
        let _ = deploy.down(&prev_dir).await;
        match deploy.deploy(&p.work_dir).await {
            Ok(r) => {
                if let Some(why) = preview_deploy_failure(deploy, &r, port).await {
                    internal_error(&format!("restore failed: {why}"))
                } else {
                    chat(format!(
                        "↩️ Preview of PR #{num} stopped — main build restored."
                    ))
                    .await;
                    Json(serde_json::json!({ "ok": true })).into_response()
                }
            }
            Err(e) => internal_error(&e.to_string()),
        }
    }
}

/// Max upload size (bytes) — generous for images/docs, bounded to protect disk.
const UPLOAD_MAX: usize = 25 * 1024 * 1024;

/// Accept a multipart file upload, store it under the project's media dir, and
/// return an [`coxagent_application::Attachment`] the client attaches to a
/// chat/discussion message. Any signed-in user may upload (same as chat).
async fn upload_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    mut multipart: axum::extract::Multipart,
) -> axum::response::Response {
    if app.project(&pid).await.is_none() {
        return not_found();
    }
    let Ok(Some(field)) = multipart.next_field().await else {
        return (StatusCode::BAD_REQUEST, "no file").into_response();
    };
    let orig = field.file_name().unwrap_or("file").to_owned();
    let mime = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_owned();
    let data = match field.bytes().await {
        Ok(b) if b.len() <= UPLOAD_MAX => b,
        Ok(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "file too large").into_response(),
        Err(_) => return (StatusCode::BAD_REQUEST, "read failed").into_response(),
    };
    let stored = format!("{}-{}", mint_media_token(), sanitize_name(&orig));
    if app
        .storage
        .put(&format!("proj/{pid}/{stored}"), &data, &mime)
        .await
        .is_err()
    {
        return internal_error("write failed");
    }
    let att = serde_json::json!({
        "name": orig,
        "url": format!("/api/projects/{pid}/media/{stored}"),
        "mime": mime,
        "size": data.len(),
    });
    Json(att).into_response()
}

/// A collision-free token for stored media filenames (nanos + a counter).
fn mint_media_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}{seq:x}")
}

/// Serve an uploaded media file. The filename is a single path segment; a guard
/// rejects any traversal, so only files inside the project's media dir are read.
async fn media_ep(
    State(app): State<AppState>,
    Path((pid, file)): Path<(String, String)>,
) -> axum::response::Response {
    if app.project(&pid).await.is_none() {
        return not_found();
    }
    if file.contains('/') || file.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let Ok(bytes) = app.storage.get(&format!("proj/{pid}/{file}")).await else {
        return not_found();
    };
    let mime = mime_of(&file);
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, max-age=31536000"),
        ],
        bytes,
    )
        .into_response()
}

/// Best-effort MIME from a file extension (for serving uploads).
fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt" | "log" | "md") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

// ── System-wide chat (hub-level: #general + project + private channels) ──────

/// Resolve the caller's username and whether they may create channels.
async fn resolve_user_caps(app: &AppState, headers: &axum::http::HeaderMap) -> (String, bool) {
    match &app.auth {
        Some(auth) => match resolve_principal(auth, headers).await {
            Some(u) => (u.username, u.role.can_create_channel()),
            None => ("user".to_owned(), false),
        },
        None => ("user".to_owned(), true), // open/local mode: allow
    }
}

/// Persist one system-chat message and fan it out to every live WebSocket.
/// Returns `false` if the user can't view the channel or persistence fails.
async fn deliver_syschat(
    app: &AppState,
    user: &str,
    body: &str,
    channel: &str,
    attachments: Vec<coxagent_application::Attachment>,
) -> bool {
    let ctx = app.chat_context().await;
    let msg = {
        let mut sc = app.syschat.inner.lock().await;
        if !sc.can_view(channel, user, &ctx) {
            return false;
        }
        sc.post(user, body, channel, attachments);
        sc.chat.last().cloned()
    };
    app.syschat.save().await;
    if let Some(m) = msg {
        let _ = app
            .syschat
            .tx
            .send(serde_json::to_string(&m).unwrap_or_default());
    }
    true
}

/// List the channels the signed-in user can see (general + their projects + private).
async fn syschat_channels_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    let channels = { app.syschat.inner.lock().await.channels_for(&user, &ctx) };
    Json(channels).into_response()
}

/// Create a private channel. Restricted to Admin + lead tier (can_create_channel).
async fn syschat_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateChannelReq>,
) -> axum::response::Response {
    let (user, can_create) = resolve_user_caps(&app, &headers).await;
    if !can_create {
        return (
            StatusCode::FORBIDDEN,
            "only leads and admins can create channels",
        )
            .into_response();
    }
    let ctx = app.chat_context().await;
    let result = {
        let mut sc = app.syschat.inner.lock().await;
        let kind = req.kind.as_deref().unwrap_or("private");
        sc.create_channel_with_kind(&req.name, &user, kind, &ctx)
    };
    match result {
        Ok(ch) => {
            app.syschat.save().await;
            (StatusCode::CREATED, Json(ch)).into_response()
        }
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

/// Invite a user to a private channel (or, with `delegate`, grant invite rights).
async fn syschat_invite_ep(
    State(app): State<AppState>,
    Path(cid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChannelMemberReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let (result, channel) = {
        let mut sc = app.syschat.inner.lock().await;
        let r = if req.delegate {
            sc.delegate(&cid, &user, &req.user)
        } else {
            sc.invite(&cid, &user, &req.user)
        };
        let ch = sc.channels.iter().find(|c| c.id == cid).cloned();
        (r, ch)
    };
    match result {
        Ok(()) => {
            app.syschat.save().await;
            Json(channel).into_response()
        }
        Err(msg) => (StatusCode::FORBIDDEN, msg).into_response(),
    }
}

/// List a channel's messages (oldest first). Empty for channels the user can't view.
async fn syschat_messages_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ChatListQuery>,
) -> axum::response::Response {
    let channel = q
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    let sc = app.syschat.inner.lock().await;
    if !sc.can_view(&channel, &user, &ctx) {
        return Json(Vec::<coxagent_application::ChatMsg>::new()).into_response();
    }
    Json(sc.messages_in(&channel)).into_response()
}

/// Post a message to a system channel over REST (WS is the primary path).
async fn syschat_send_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PostChatReq>,
) -> axum::response::Response {
    let body = req.body.trim();
    if body.is_empty() && req.attachments.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty message").into_response();
    }
    if body.chars().count() > CHAT_MAX_CHARS {
        return (StatusCode::PAYLOAD_TOO_LARGE, "message too long").into_response();
    }
    let user = resolve_username(&app, &headers).await;
    let channel = req
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    if deliver_syschat(&app, &user, body, &channel, req.attachments).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::FORBIDDEN, "cannot post to this channel").into_response()
    }
}

/// List workspace members (minimal fields) for @mentions, member lists, and DMs.
/// Available to any signed-in user.
async fn syschat_members_ep(State(app): State<AppState>) -> axum::response::Response {
    let members: Vec<serde_json::Value> = match &app.auth {
        Some(a) => a
            .list_users()
            .await
            .into_iter()
            .map(|u| serde_json::json!({ "username": u.username, "name": u.name }))
            .collect(),
        None => Vec::new(),
    };
    Json(members).into_response()
}

#[derive(serde::Deserialize)]
struct DmReq {
    user: String,
}

/// Open (or fetch) a direct-message channel with another user.
async fn syschat_dm_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<DmReq>,
) -> axum::response::Response {
    let me = resolve_username(&app, &headers).await;
    let result = { app.syschat.inner.lock().await.open_dm(&me, req.user.trim()) };
    match result {
        Ok(ch) => {
            app.syschat.save().await;
            (StatusCode::CREATED, Json(ch)).into_response()
        }
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct ReactReq {
    id: String,
    emoji: String,
}

/// Toggle the caller's emoji reaction on a message; broadcast the update.
async fn syschat_react_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ReactReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let emoji = req.emoji.chars().take(8).collect::<String>();
    let updated = { app.syschat.inner.lock().await.react(&req.id, &user, &emoji) };
    let Some(msg) = updated else {
        return not_found();
    };
    app.syschat.save().await;
    // Broadcast a reaction event so every client updates the message in place.
    let evt = serde_json::json!({
        "type": "reaction", "channel": msg.channel, "id": msg.id, "reactions": msg.reactions,
    });
    let _ = app.syschat.tx.send(evt.to_string());
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[derive(serde::Deserialize)]
struct WebhookReq {
    channel: String,
    #[serde(default)]
    label: String,
}

/// Create an incoming webhook for a channel (any member). Returns the token +
/// the full post URL.
async fn syschat_webhook_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<WebhookReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    let wh = {
        let mut sc = app.syschat.inner.lock().await;
        if !sc.can_view(&req.channel, &user, &ctx) {
            return (StatusCode::FORBIDDEN, "not a member of this channel").into_response();
        }
        sc.create_webhook(&req.channel, &req.label)
    };
    app.syschat.save().await;
    Json(serde_json::json!({
        "token": wh.token, "channel": wh.channel, "label": wh.label,
        "url": format!("/api/chat/hook/{}", wh.token),
    }))
    .into_response()
}

/// List a channel's webhooks (members only).
async fn syschat_webhooks_list_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ChatListQuery>,
) -> axum::response::Response {
    let channel = q
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    let sc = app.syschat.inner.lock().await;
    if !sc.can_view(&channel, &user, &ctx) {
        return Json(Vec::<coxagent_application::Webhook>::new()).into_response();
    }
    Json(sc.webhooks_for(&channel)).into_response()
}

/// Revoke a webhook by token.
async fn syschat_webhook_delete_ep(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> axum::response::Response {
    let removed = { app.syschat.inner.lock().await.revoke_webhook(&token) };
    if removed {
        app.syschat.save().await;
    }
    Json(serde_json::json!({ "ok": removed })).into_response()
}

// ── Threads ──────────────────────────────────────────────────────────────
async fn syschat_reply_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PostChatReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    if user.is_empty() {
        return (StatusCode::UNAUTHORIZED, "sign in").into_response();
    }
    let body = body.body.trim().to_owned();
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty message").into_response();
    }
    if body.len() > CHAT_MAX_CHARS {
        return (StatusCode::PAYLOAD_TOO_LARGE, "too long").into_response();
    }
    let mid_clone = mid.clone();
    let mut sc = app.syschat.inner.lock().await;
    let channel = match sc
        .chat
        .iter()
        .find(|m| m.id == mid_clone)
        .map(|p| p.channel.clone())
    {
        Some(ch) => ch,
        None => return (StatusCode::NOT_FOUND, "parent not found").into_response(),
    };
    let msg = ChatMsg::reply(&user, &body, &channel, &mid_clone);
    sc.chat.push(msg.clone());
    if let Some(p) = sc.chat.iter_mut().find(|m| m.id == mid_clone) {
        p.reply_count = p.reply_count.saturating_add(1);
    }

    drop(sc);
    app.syschat.save().await;
    let frame = json!({ "op": "msg", "msg": msg });
    let _ = app.syschat.tx.send(frame.to_string());
    (StatusCode::CREATED, Json(msg)).into_response()
}
async fn syschat_thread_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
) -> axum::response::Response {
    let sc = app.syschat.inner.lock().await;
    let parent = sc.chat.iter().find(|m| m.id == mid);
    let _channel = match parent {
        Some(p) => p.channel.clone(),
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let replies: Vec<ChatMsg> = sc
        .chat
        .iter()
        .filter(|m| m.thread_id.as_deref() == Some(&mid))
        .cloned()
        .collect();
    Json(serde_json::json!({ "parent": parent, "replies": replies })).into_response()
}

// ── Edit / Delete ────────────────────────────────────────────────────────
async fn syschat_edit_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PostChatReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    if user.is_empty() {
        return (StatusCode::UNAUTHORIZED, "sign in").into_response();
    }
    let new_body = body.body.trim().to_owned();
    if new_body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty").into_response();
    }
    let mut sc = app.syschat.inner.lock().await;
    let Some(msg) = sc.chat.iter_mut().find(|m| m.id == mid) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if msg.user != user {
        return (StatusCode::FORBIDDEN, "not yours").into_response();
    }
    msg.body = new_body;
    msg.edited = Some(now_rfc3339());
    let edited = msg.clone();
    drop(sc);
    app.syschat.save().await;
    let frame = json!({ "op": "edit", "msg": edited });
    let _ = app.syschat.tx.send(frame.to_string());
    Json(edited).into_response()
}
async fn syschat_delete_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    if user.is_empty() {
        return (StatusCode::UNAUTHORIZED, "sign in").into_response();
    }
    let mut sc = app.syschat.inner.lock().await;
    let Some(msg) = sc.chat.iter_mut().find(|m| m.id == mid) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if msg.user != user {
        return (StatusCode::FORBIDDEN, "not yours").into_response();
    }
    msg.deleted = true;
    msg.body = String::new();
    drop(sc);
    app.syschat.save().await;
    let frame = json!({ "op": "delete", "msg": { "id": mid } });
    let _ = app.syschat.tx.send(frame.to_string());
    Json(serde_json::json!({ "ok": true })).into_response()
}

// ── Search ───────────────────────────────────────────────────────────────
async fn syschat_search_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    let term = q.get("q").map(|s| s.to_lowercase()).unwrap_or_default();
    if term.is_empty() {
        return Json(Vec::<ChatMsg>::new()).into_response();
    }
    let sc = app.syschat.inner.lock().await;
    let results: Vec<ChatMsg> = sc
        .chat
        .iter()
        .filter(|m| {
            !m.deleted
                && sc.can_view(&m.channel, &user, &ctx)
                && (m.body.to_lowercase().contains(&term) || m.user.to_lowercase().contains(&term))
        })
        .rev()
        .take(50)
        .cloned()
        .collect();
    Json(results).into_response()
}

// ── Pin ──────────────────────────────────────────────────────────────────
async fn syschat_pin_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    if user.is_empty() {
        return (StatusCode::UNAUTHORIZED, "sign in").into_response();
    }
    let mid_clone = mid.clone();
    let mut sc = app.syschat.inner.lock().await;
    let channel = match sc
        .chat
        .iter()
        .find(|m| m.id == mid_clone)
        .map(|m| m.channel.clone())
    {
        Some(ch) => ch,
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let pins = sc.pins.entry(channel.clone()).or_default();
    if pins.contains(&mid_clone) {
        pins.retain(|p| p != &mid_clone);
    } else {
        pins.push(mid_clone.clone());
    }
    let pinned = pins.contains(&mid_clone);
    let pins_clone = pins.clone();
    drop(sc);
    app.syschat.save().await;
    let _ = app
        .syschat
        .tx
        .send(json!({ "op": "pin", "channel": channel, "pins": pins_clone }).to_string());
    Json(serde_json::json!({ "ok": true, "pinned": pinned })).into_response()
}
async fn syschat_pins_ep(
    State(app): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let ch = q.get("channel").cloned().unwrap_or_default();
    let sc = app.syschat.inner.lock().await;
    let pins = sc.pins.get(&ch).cloned().unwrap_or_default();
    let msgs: Vec<ChatMsg> = pins
        .iter()
        .filter_map(|id| {
            sc.chat
                .iter()
                .find(|m| m.id == *id && !m.deleted && m.channel == ch)
        })
        .cloned()
        .collect();
    Json(msgs).into_response()
}

// ── Topic ──────────────────────────────────────────────────────────────────
#[derive(serde::Deserialize)]
struct TopicReq {
    topic: String,
}
async fn syschat_topic_ep(
    State(app): State<AppState>,
    Path(cid): Path<String>,
    Json(body): Json<TopicReq>,
) -> axum::response::Response {
    let topic = body.topic.trim().to_owned();
    let mut sc = app.syschat.inner.lock().await;
    sc.topic(&cid, topic);
    drop(sc);
    app.syschat.save().await;
    Json(serde_json::json!({"ok":true})).into_response()
}
async fn syschat_topic_get_ep(
    State(app): State<AppState>,
    Path(cid): Path<String>,
) -> axum::response::Response {
    let sc = app.syschat.inner.lock().await;
    let topic = sc.get_topic(&cid);
    Json(serde_json::json!({"topic": topic})).into_response()
}

#[derive(serde::Deserialize)]
struct HookPostReq {
    #[serde(default)]
    text: String,
    #[serde(default)]
    username: String,
}

/// Public webhook endpoint: an external system posts a message to a channel
/// using only the secret token (no login). The token is the credential.
async fn syschat_hook_ep(
    State(app): State<AppState>,
    Path(token): Path<String>,
    Json(req): Json<HookPostReq>,
) -> axum::response::Response {
    let Some((channel, label)) = ({ app.syschat.inner.lock().await.webhook(&token) }) else {
        return not_found();
    };
    let text = req.text.trim();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty text").into_response();
    }
    let author = if req.username.trim().is_empty() {
        label
    } else {
        req.username.trim().to_owned()
    };
    let msg = {
        let mut sc = app.syschat.inner.lock().await;
        let id = sc.post(
            &author,
            &text.chars().take(4000).collect::<String>(),
            &channel,
            Vec::new(),
        );
        sc.chat.iter().find(|m| m.id == id).cloned()
    };
    app.syschat.save().await;
    if let Some(m) = msg {
        let _ = app
            .syschat
            .tx
            .send(serde_json::to_string(&m).unwrap_or_default());
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// WebRTC ICE servers for calls: a public STUN server, plus a TURN relay with
/// short-lived HMAC credentials when `COXAGENT_TURN_URL`/`_SECRET` are set
/// (coturn's `use-auth-secret` REST scheme). TURN lets calls traverse NATs that
/// block direct peer connections.
async fn ice_config_ep() -> axum::response::Response {
    let mut servers = vec![serde_json::json!({ "urls": "stun:stun.l.google.com:19302" })];
    if let (Ok(url), Ok(secret)) = (
        std::env::var("COXAGENT_TURN_URL"),
        std::env::var("COXAGENT_TURN_SECRET"),
    ) {
        let ttl: u64 = std::env::var("COXAGENT_TURN_TTL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3600);
        let exp = now_unix_secs() + ttl;
        let username = format!("{exp}:cox");
        let credential = base64_std(&hmac_sha1(secret.as_bytes(), username.as_bytes()));
        // Offer the relay over both UDP and TCP for reachability.
        let base = url.trim_end_matches("?transport=udp").to_owned();
        servers.push(serde_json::json!({
            "urls": [format!("{base}?transport=udp"), format!("{base}?transport=tcp")],
            "username": username,
            "credential": credential,
        }));
    }
    Json(serde_json::json!({ "iceServers": servers })).into_response()
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// HMAC-SHA1 (coturn's long-term-credential scheme).
fn hmac_sha1(key: &[u8], msg: &[u8]) -> Vec<u8> {
    use hmac::Mac;
    let Ok(mut mac) = hmac::Hmac::<sha1::Sha1>::new_from_slice(key) else {
        return Vec::new();
    };
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// Standard Base64 (for the TURN credential).
fn base64_std(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(A[(b0 >> 2) as usize] as char);
        out.push(A[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Whether `user` may receive a system-chat broadcast (membership per message).
async fn syschat_may_see(app: &AppState, user: &str, json: &str) -> bool {
    let channel = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("channel").and_then(|c| c.as_str()).map(str::to_owned))
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    if channel == coxagent_application::GENERAL_CHANNEL {
        return true;
    }
    let ctx = app.chat_context().await;
    app.syschat
        .inner
        .lock()
        .await
        .can_view(&channel, user, &ctx)
}

/// Live system-chat WebSocket (same-origin guarded, authenticated).
async fn syschat_ws_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Some(resp) = role_guard(true) {
        return resp;
    }
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    let user = match &app.auth {
        Some(auth) => match resolve_principal(auth, &headers).await {
            Some(u) => u.username,
            None => return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response(),
        },
        None => "user".to_owned(),
    };
    let ws = ws.max_message_size(64 * 1024);
    ws.on_upgrade(move |socket| syschat_socket(socket, app, user))
}

/// Drive one system-chat WebSocket: forward broadcasts the user may see, accept
/// validated, rate-limited messages from the client.
async fn syschat_socket(mut socket: WebSocket, app: AppState, user: String) {
    let mut rx = app.syschat.tx.subscribe();
    let mut recv_times: std::collections::VecDeque<std::time::Instant> =
        std::collections::VecDeque::new();
    loop {
        tokio::select! {
            bcast = rx.recv() => {
                match bcast {
                    Ok(json) => {
                        // Call-signaling messages are addressed to one user; chat
                        // messages use channel membership.
                        let route_ok = match serde_json::from_str::<serde_json::Value>(&json) {
                            Ok(v) if v.get("type").and_then(|t| t.as_str()) == Some("signal") =>
                                v.get("to").and_then(|t| t.as_str()) == Some(user.as_str()),
                            _ => syschat_may_see(&app, &user, &json).await,
                        };
                        if !route_ok { continue; }
                        if socket.send(Message::Text(json)).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue,
                };
                let parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
                // WebRTC call signaling: stamp the sender and relay 1:1 to the
                // addressed user. Not persisted, not rate-limited.
                if parsed.as_ref().and_then(|v| v.get("type")).and_then(|t| t.as_str()) == Some("signal") {
                    if let Some(mut v) = parsed.clone() {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("from".to_owned(), serde_json::Value::String(user.clone()));
                            let _ = app.syschat.tx.send(v.to_string());
                        }
                    }
                    continue;
                }
                // Typing indicators: broadcast to other users, don't persist.
                if parsed.as_ref().and_then(|v| v.get("op")).and_then(|o| o.as_str()) == Some("typing") {
                    if let Some(mut v) = parsed.clone() {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("user".to_owned(), serde_json::Value::String(user.clone()));
                            let _ = app.syschat.tx.send(v.to_string());
                        }
                    }
                    continue;
                }
                let body = parsed.as_ref()
                    .and_then(|v| v.get("body").and_then(|b| b.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| text.clone());
                let channel = parsed.as_ref()
                    .and_then(|v| v.get("channel").and_then(|c| c.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
                let attachments: Vec<coxagent_application::Attachment> = parsed.as_ref()
                    .and_then(|v| v.get("attachments").cloned())
                    .and_then(|a| serde_json::from_value(a).ok())
                    .unwrap_or_default();
                let body = body.trim();
                if (body.is_empty() && attachments.is_empty()) || body.chars().count() > CHAT_MAX_CHARS {
                    continue;
                }
                let now = std::time::Instant::now();
                while recv_times.front().is_some_and(|t| now.duration_since(*t) > CHAT_RATE_WINDOW) {
                    recv_times.pop_front();
                }
                if recv_times.len() >= CHAT_RATE_MAX { continue; }
                recv_times.push_back(now);
                deliver_syschat(&app, &user, body, &channel, attachments).await;
            }
        }
    }
}

/// Upload a file for system chat; stored via the blob store (disk or S3/MinIO).
async fn syschat_upload_ep(
    State(app): State<AppState>,
    mut multipart: axum::extract::Multipart,
) -> axum::response::Response {
    let Ok(Some(field)) = multipart.next_field().await else {
        return (StatusCode::BAD_REQUEST, "no file").into_response();
    };
    let orig = field.file_name().unwrap_or("file").to_owned();
    let mime = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_owned();
    let data = match field.bytes().await {
        Ok(b) if b.len() <= UPLOAD_MAX => b,
        Ok(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "file too large").into_response(),
        Err(_) => return (StatusCode::BAD_REQUEST, "read failed").into_response(),
    };
    let stored = format!("{}-{}", mint_media_token(), sanitize_name(&orig));
    if app
        .storage
        .put(&format!("chat/{stored}"), &data, &mime)
        .await
        .is_err()
    {
        return internal_error("write failed");
    }
    Json(serde_json::json!({
        "name": orig,
        "url": format!("/api/chat/media/{stored}"),
        "mime": mime,
        "size": data.len(),
    }))
    .into_response()
}

/// Serve a system-chat media file from the blob store (path-traversal guarded).
async fn syschat_media_ep(
    State(app): State<AppState>,
    Path(file): Path<String>,
) -> axum::response::Response {
    if file.contains('/') || file.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let Ok(bytes) = app.storage.get(&format!("chat/{file}")).await else {
        return not_found();
    };
    (
        [
            (header::CONTENT_TYPE, mime_of(&file)),
            (header::CACHE_CONTROL, "private, max-age=31536000"),
        ],
        bytes,
    )
        .into_response()
}

/// Sanitize an original filename to a safe stored suffix (keeps the extension).
fn sanitize_name(orig: &str) -> String {
    orig.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
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
    if let Some(resp) = role_guard(true) {
        return resp;
    }
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
            // Server → client: forward a broadcast message, but never leak a
            // private channel to a non-member — check membership per message.
            bcast = rx.recv() => {
                match bcast {
                    Ok(json) => {
                        if !user_may_see_broadcast(&p, &user, &json).await {
                            continue;
                        }
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
                // Accept a raw string or {"body": "...", "attachments": [...]}.
                let parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
                let body = parsed
                    .as_ref()
                    .and_then(|v| v.get("body").and_then(|b| b.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| text.clone());
                let channel = parsed
                    .as_ref()
                    .and_then(|v| v.get("channel").and_then(|c| c.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
                let attachments: Vec<coxagent_application::Attachment> = parsed
                    .as_ref()
                    .and_then(|v| v.get("attachments").cloned())
                    .and_then(|a| serde_json::from_value(a).ok())
                    .unwrap_or_default();
                let body = body.trim();
                if (body.is_empty() && attachments.is_empty()) || body.chars().count() > CHAT_MAX_CHARS {
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
                deliver_chat(&app, &p, &user, body, &channel, attachments).await;
            }
        }
    }
}

/// Whether `user` may receive a broadcast chat message. `#general` (and any
/// message with no channel tag) is open; private channels require membership,
/// checked against current state so a live invite takes effect immediately.
async fn user_may_see_broadcast(p: &ProjectHandle, user: &str, json: &str) -> bool {
    let channel = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("channel").and_then(|c| c.as_str()).map(str::to_owned))
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    if channel == coxagent_application::GENERAL_CHANNEL {
        return true;
    }
    match p.store.load().await {
        Ok(state) => state.channel(&channel).is_some_and(|c| c.can_view(user)),
        Err(_) => false,
    }
}

/// SM-run standup: posts a deterministic status roundup to the team channel and
/// pulls in each agent's latest contribution. Zero engine cost — derived from
/// state — so it can be triggered freely to see the team "gather".
/// Current sprint number (0 when not in a sprint), for review labels.
async fn current_sprint(p: &ProjectHandle) -> u32 {
    p.store
        .load()
        .await
        .ok()
        .and_then(|s| s.sprint.map(|sp| sp.number))
        .unwrap_or(0)
}

#[derive(serde::Deserialize)]
struct ChatReplyReq {
    message: String,
}

/// A human posted in the team channel — the most relevant agent replies
/// intelligently and runs any action requested. Fired by the composer.
async fn chat_reply_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<ChatReplyReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let msg = req.message.trim();
    if msg.is_empty() {
        return Json(serde_json::json!({ "ok": true })).into_response();
    }
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    let mut uc = coxagent_application::use_cases::RunChatReplyUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
        cfg.workflow.token_saver,
        cfg.workflow.language,
    );
    if let Some(d) = &p.deploy {
        uc = uc
            .with_deploy(Arc::clone(d))
            .with_host_port(cfg.deploy.host_port);
    }
    if let Some(f) = &p.forge {
        let target = if cfg.git.target_branch.trim().is_empty() {
            cfg.git.default_branch.clone()
        } else {
            cfg.git.target_branch.clone()
        };
        uc = uc.with_forge(Arc::clone(f), target, cfg.git.require_ci);
    }
    match uc.execute(msg).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// On-demand SA architecture review: files refactor chores + a PO nudge.
async fn architecture_review_ep(
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
    let uc = coxagent_application::use_cases::RunArchitectureAuditUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
        cfg.workflow.token_saver,
        cfg.workflow.language,
    );
    match uc.execute(current_sprint(&p).await).await {
        Ok(filed) => Json(serde_json::json!({ "ok": true, "filed": filed })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// On-demand DOCS Wiki-gap review: writes missing pages in full.
async fn docs_review_ep(
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
    let uc = coxagent_application::use_cases::RunDocsAuditUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
        cfg.workflow.language,
    );
    match uc.execute(current_sprint(&p).await).await {
        Ok(written) => Json(serde_json::json!({ "ok": true, "written": written })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// The Scrum language configured for a project (English by default).
fn project_language(p: &ProjectHandle) -> coxagent_application::config::Language {
    std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .map_or(coxagent_application::config::Language::En, |c| {
            c.workflow.language
        })
}

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

#[derive(serde::Deserialize)]
struct AgentLogQuery {
    role: String,
    /// Optional operator (`account@host` or just the account) to view that
    /// specific worker's live log when several run the same role.
    #[serde(default)]
    worker: String,
}

/// Live agent log for a role: the streamed `<workspace>/logs/live/<role>.log`
/// (updated during the run), falling back to the newest completed transcript
/// for that role. Powers the full-screen live agent view.
async fn agent_log_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<AgentLogQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Sanitize the role to a filename token (no path traversal).
    let role: String = q
        .role
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if role.is_empty() {
        return (StatusCode::BAD_REQUEST, "role required").into_response();
    }
    // A specific operator's log is `<role>__<account>.log`; without a worker (or
    // when that file is absent) fall back to the shared `<role>.log`.
    let account: String = q
        .worker
        .split('@')
        .next()
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let base = p.config_path.parent().unwrap_or(&p.config_path);
    let live_dir = base.join("logs").join("live");
    let per_op = live_dir.join(format!("{role}__{account}.log"));
    let live = if !account.is_empty() && per_op.exists() {
        per_op
    } else {
        live_dir.join(format!("{role}.log"))
    };
    // Local live file first (this machine's operators). If empty/absent, try
    // shared storage (MinIO) where remote operators mirror their live logs, so
    // the central hub can show an operator running on another machine.
    let local = std::fs::read_to_string(&live)
        .ok()
        .filter(|s| s.trim().len() > 20);
    let remote = if local.is_some() {
        None
    } else {
        let name = if account.is_empty() {
            format!("{role}.log")
        } else {
            format!("{role}__{account}.log")
        };
        app.storage
            .get(&format!("agentlogs/{pid}/{name}"))
            .await
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|s| s.trim().len() > 20)
    };
    let (body, live_flag) = if let Some(s) = local.or(remote) {
        (s, true)
    } else {
        // Fall back to the latest transcript for this role.
        let dir = transcripts_dir(&p);
        let latest = std::fs::read_dir(&dir).ok().and_then(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().contains(&role))
                .max_by_key(|e| {
                    e.metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                })
                .map(|e| e.path())
        });
        let text = latest
            .and_then(|pth| std::fs::read_to_string(pth).ok())
            .unwrap_or_default();
        (text, false)
    };
    Json(serde_json::json!({ "role": role, "live": live_flag, "log": body })).into_response()
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

/// This machine's hostname (the "machine" the agents run on), or `"local"`.
fn machine_host() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".to_owned())
}

async fn control_ep(
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
    match action.as_str() {
        "resume" => {
            p.runner.set_operator(&account, &machine_host());
            p.runner.resume();
            // Persist this operator's intent so reopening the app auto-resumes
            // for THIS user only — never starts anyone else's operator.
            let _ = p.store.set_desired(&operator, true).await;
        }
        // Pause/stop are local to this operator and persist the stopped intent,
        // so a reopen stays idle instead of auto-resuming.
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
async fn operator_control_ep(
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
        let allowed = caller
            .as_ref()
            .is_some_and(|u| u.role.can_manage() || u.username.eq_ignore_ascii_case(account));
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

/// Post an on-demand daily digest (shipped/spend/sprint at a glance) into the
/// project's team chat and return it — the `/digest` slash command.
async fn digest_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let now = coxagent_application::state::now_rfc3339();
    let digest = coxagent_application::metrics::digest_markdown(&state, &now);
    drop(state);
    let res = coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        s.post_chat_in(
            "COX",
            &format!("📰 {digest}"),
            coxagent_application::state::AGENTS_CHANNEL,
            Vec::new(),
        );
        Ok(())
    })
    .await;
    match res {
        Ok(()) => Json(serde_json::json!({ "ok": true, "digest": digest })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

// ---------------- Workspace: identity, invites, overview, my-agents ----------

async fn workspace_get_ep(State(app): State<AppState>) -> axum::response::Response {
    let w = app.workspace.inner.lock().await.clone();
    Json(serde_json::json!({
        "name": w.name, "tagline": w.tagline, "accent": w.accent, "conventions": w.conventions,
        "configured": !w.name.trim().is_empty(),
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct WorkspacePutReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    tagline: String,
    #[serde(default)]
    accent: String,
    #[serde(default)]
    conventions: Option<String>,
    /// Download/release config — only overwritten when provided.
    #[serde(default)]
    downloads: Option<DownloadsCfg>,
}

/// Set the workspace identity (admin — writes are admin-gated by middleware).
async fn workspace_put_ep(
    State(app): State<AppState>,
    Json(req): Json<WorkspacePutReq>,
) -> axum::response::Response {
    {
        let mut w = app.workspace.inner.lock().await;
        req.name.trim().clone_into(&mut w.name);
        req.tagline.trim().clone_into(&mut w.tagline);
        req.accent.trim().clone_into(&mut w.accent);
        // Conventions edited on their own screen; only overwrite when provided.
        if let Some(c) = &req.conventions {
            c.trim().clone_into(&mut w.conventions);
        }
        if let Some(d) = req.downloads {
            w.downloads = d;
        }
    }
    app.workspace.save().await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[derive(serde::Deserialize)]
struct InviteCreateReq {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    uses: Option<u32>,
}

/// Mint a shareable invite link (admin). Whoever opens it self-registers with
/// the preset role + project membership.
async fn invite_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<InviteCreateReq>,
) -> axum::response::Response {
    let by = resolve_username(&app, &headers).await;
    // A normal admin may only invite into projects of THEIR spaces; the super
    // admin is unrestricted. (No spaces defined yet = legacy single-space mode,
    // unrestricted for any admin.)
    if !is_super(&app, &headers).await {
        let mine = spaces_for(&app, &headers).await;
        let has_spaces = !app.spaces.inner.lock().await.spaces.is_empty();
        if has_spaces {
            let allowed: std::collections::HashSet<&String> =
                mine.iter().flat_map(|s| s.projects.iter()).collect();
            if req.projects.iter().any(|p| !allowed.contains(p)) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"error":"you can only invite into your own space's projects"})),
                )
                    .into_response();
            }
        }
    }
    let token = format!(
        "{}{}",
        coxagent_application::state::mint_id(),
        coxagent_application::state::mint_id()
    );
    let invite = Invite {
        token: token.clone(),
        role: req.role.unwrap_or_else(|| "viewer".to_owned()),
        projects: req.projects,
        created_by: by,
        created_at: coxagent_application::state::now_rfc3339(),
        uses_left: req.uses.unwrap_or(5).clamp(1, 100),
    };
    {
        let mut w = app.workspace.inner.lock().await;
        w.invites.push(invite);
    }
    app.workspace.save().await;
    Json(serde_json::json!({ "ok": true, "token": token, "url": format!("/join/{token}") }))
        .into_response()
}

async fn invites_list_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Some(auth) = app.auth.clone() {
        let admin = resolve_principal(&auth, &headers)
            .await
            .is_some_and(|u| u.role.can_manage());
        if !admin {
            return (StatusCode::FORBIDDEN, "admin role required").into_response();
        }
    }
    let w = app.workspace.inner.lock().await.clone();
    Json(w.invites).into_response()
}

async fn invite_delete_ep(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> axum::response::Response {
    {
        let mut w = app.workspace.inner.lock().await;
        w.invites.retain(|i| i.token != token);
    }
    app.workspace.save().await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// The public join page an invitee lands on: a minimal branded form that
/// registers their account against the invite token.
async fn join_page_ep(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> axum::response::Response {
    let w = app.workspace.inner.lock().await.clone();
    let valid = w
        .invites
        .iter()
        .any(|i| i.token == token && i.uses_left > 0);
    let name = if w.name.trim().is_empty() {
        "CoXAgent".to_owned()
    } else {
        w.name
    };
    let body = if valid {
        format!(
            r#"<h1>Join {n}</h1><p class="sub">Create your account to join the workspace.</p>
<input id="u" placeholder="username" autocomplete="username">
<input id="n" placeholder="display name (optional)">
<input id="p" type="password" placeholder="password" autocomplete="new-password">
<button onclick="go()">Join workspace</button><div id="err"></div>
<script>async function go(){{const r=await fetch('/api/workspace/join',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{token:'{t}',username:u.value.trim(),password:p.value,name:n.value.trim()}})}});if(r.ok)location.href='/';else document.getElementById('err').textContent=(await r.json().catch(()=>({{}}))).error||'could not join';}}
document.addEventListener('keydown',e=>{{if(e.key==='Enter')go()}});</script>"#,
            n = html_escape(&name),
            t = html_escape(&token),
        )
    } else {
        format!(
            "<h1>{}</h1><p class=\"sub\">This invite link is invalid or has been used up. Ask an admin for a new one.</p>",
            html_escape(&name)
        )
    };
    axum::response::Html(format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Join</title>
<style>body{{font-family:-apple-system,system-ui,sans-serif;background:#0b1020;color:#e6ecff;display:grid;place-items:center;min-height:100vh;margin:0}}
.card{{background:#121a30;border:1px solid #24304f;border-radius:16px;padding:34px;width:340px;box-shadow:0 20px 60px rgba(0,0,0,.4)}}
h1{{font-size:20px;margin:0 0 6px}} .sub{{color:#8b98b8;font-size:13px;margin:0 0 18px}}
input{{width:100%;box-sizing:border-box;background:#0b1020;color:#e6ecff;border:1px solid #24304f;border-radius:10px;padding:11px 13px;font-size:14px;margin-bottom:10px}}
button{{width:100%;background:#22d3ee;color:#06202a;border:none;border-radius:10px;padding:12px;font-size:14px;font-weight:700;cursor:pointer}}
#err{{color:#f87171;font-size:12.5px;margin-top:10px;min-height:16px}}</style>
<div class="card">{body}</div>"#
    ))
    .into_response()
}

#[derive(serde::Deserialize)]
struct JoinReq {
    token: String,
    username: String,
    password: String,
    #[serde(default)]
    name: String,
}

/// Redeem an invite: create the account with the invite's role + projects,
/// consume one use, and sign the new member straight in.
async fn join_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<JoinReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({"error":"auth not configured"})),
        )
            .into_response();
    };
    let username = req.username.trim().to_owned();
    if username.is_empty() || req.password.len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"username and a password (8+ chars) required"})),
        )
            .into_response();
    }
    // Validate + consume one use atomically under the workspace lock.
    let invite = {
        let mut w = app.workspace.inner.lock().await;
        let Some(i) = w
            .invites
            .iter_mut()
            .find(|i| i.token == req.token && i.uses_left > 0)
        else {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error":"invite invalid or used up"})),
            )
                .into_response();
        };
        i.uses_left -= 1;
        let inv = i.clone();
        w.invites.retain(|i| i.uses_left > 0);
        inv
    };
    app.workspace.save().await;
    let role = role_from(Some(invite.role.as_str()));
    if !auth.create_user(&username, &req.password, role).await {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":"username already taken"})),
        )
            .into_response();
    }
    if !req.name.trim().is_empty() {
        auth.update_user(&username, req.name.trim(), "", None).await;
    }
    for pid in &invite.projects {
        auth.assign_project(&username, pid).await;
    }
    audit_push(&app.audit, &username, "joined via invite".to_owned(), 200).await;
    // Sign them straight in (no 2FA on a brand-new account).
    match auth.login(&username, &req.password, None).await {
        coxagent_application::LoginResult::Ok(token) => {
            let cookie = format!(
                "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200{}",
                cookie_secure(&headers)
            );
            (
                [(header::SET_COOKIE, cookie)],
                Json(serde_json::json!({"ok":true})),
            )
                .into_response()
        }
        _ => Json(serde_json::json!({ "ok": true, "login": "manual" })).into_response(),
    }
}

// ---------------- Spaces (multi-workspace) + Manage --------------------------

/// Whether the caller is the hub super admin.
async fn is_super(app: &AppState, headers: &axum::http::HeaderMap) -> bool {
    match app.auth.clone() {
        Some(auth) => resolve_principal(&auth, headers)
            .await
            .is_some_and(|u| u.role.is_super()),
        // Open mode (no auth): single-user local — allow.
        None => true,
    }
}

/// The spaces the caller may see: all for a super admin; otherwise the ones
/// they administer or hold a member project in.
async fn spaces_for(app: &AppState, headers: &axum::http::HeaderMap) -> Vec<Space> {
    let all = app.spaces.inner.lock().await.spaces.clone();
    if is_super(app, headers).await {
        return all;
    }
    let (me, my_projects) = match app.auth.clone() {
        Some(auth) => resolve_principal(&auth, headers)
            .await
            .map_or((String::new(), Vec::new()), |u| (u.username, u.projects)),
        None => return all,
    };
    all.into_iter()
        .filter(|s| {
            s.admins.iter().any(|a| a.eq_ignore_ascii_case(&me))
                || s.members.iter().any(|m| m.eq_ignore_ascii_case(&me))
                || s.projects.iter().any(|p| my_projects.contains(p))
        })
        .collect()
}

async fn spaces_list_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let visible = spaces_for(&app, &headers).await;
    let sup = is_super(&app, &headers).await;
    Json(serde_json::json!({ "spaces": visible, "super": sup })).into_response()
}

#[derive(serde::Deserialize)]
struct SpaceReq {
    name: String,
    #[serde(default)]
    tagline: String,
    #[serde(default)]
    admins: Vec<String>,
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    members: Vec<String>,
    /// Monthly USD cap (0 = none). Applied by Super only.
    #[serde(default)]
    budget_usd: f64,
}

/// Validate a space payload against reality: length caps, admins must be real
/// accounts, projects must be registered — a typo must fail loudly, not create
/// silently-broken scoping. Returns the normalized (deduped) lists.
async fn validate_space_req(
    app: &AppState,
    req: &SpaceReq,
    exclude_sid: Option<&str>,
) -> Result<(Vec<String>, Vec<String>, Vec<String>), String> {
    if req.name.trim().chars().count() > 60 {
        return Err("name too long (max 60)".into());
    }
    if req.tagline.chars().count() > 160 {
        return Err("tagline too long (max 160)".into());
    }
    if req.admins.len() > 20 || req.projects.len() > 100 || req.members.len() > 200 {
        return Err("too many admins/projects/members".into());
    }
    let known_users: std::collections::HashSet<String> = match app.auth.clone() {
        Some(auth) => auth
            .list_users()
            .await
            .into_iter()
            .map(|u| u.username.to_lowercase())
            .collect(),
        None => std::collections::HashSet::new(),
    };
    let valid_users = |list: &[String]| -> Result<Vec<String>, String> {
        let mut out: Vec<String> = Vec::new();
        for a in list {
            let a = a.trim().to_owned();
            if a.is_empty() || out.contains(&a) {
                continue;
            }
            if !known_users.is_empty() && !known_users.contains(&a.to_lowercase()) {
                return Err(format!("unknown user: {a}"));
            }
            out.push(a);
        }
        Ok(out)
    };
    let admins = valid_users(&req.admins)?;
    let members = valid_users(&req.members)?;
    let known_projects: std::collections::HashSet<String> =
        app.order.read().await.iter().cloned().collect();
    let mut projects = Vec::new();
    for p in &req.projects {
        let p = p.trim().to_owned();
        if p.is_empty() || projects.contains(&p) {
            continue;
        }
        if !known_projects.contains(&p) {
            return Err(format!("unknown project: {p}"));
        }
        projects.push(p);
    }
    // A project belongs to exactly ONE space — claiming one already filed in a
    // different space must fail loudly, not silently double-book it.
    {
        let doc = app.spaces.inner.lock().await;
        for p in &projects {
            if let Some(owner) = doc
                .spaces
                .iter()
                .find(|s| Some(s.id.as_str()) != exclude_sid && s.projects.contains(p))
            {
                return Err(format!("project {p} already belongs to space {}", owner.id));
            }
        }
    }
    Ok((admins, projects, members))
}

/// Adding someone to a space means they can actually WORK there: each member
/// is assigned to every project of the space (additive only — removing a
/// member from the space never auto-revokes project access; that stays an
/// explicit per-project action in Users).
async fn assign_space_members(app: &AppState, members: &[String], projects: &[String]) {
    let Some(auth) = app.auth.clone() else { return };
    for m in members {
        for p in projects {
            let _ = auth.assign_project(m, p).await;
        }
    }
}

/// Create a space (super admin only).
async fn space_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SpaceReq>,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "super admin required").into_response();
    }
    let name = req.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name required").into_response();
    }
    let (admins, projects, members) = match validate_space_req(&app, &req, None).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let id = coxagent_application::state::slugify(&name);
    let by = resolve_username(&app, &headers).await;
    {
        let mut doc = app.spaces.inner.lock().await;
        if doc.spaces.iter().any(|s| s.id == id) {
            return (StatusCode::CONFLICT, "space id already exists").into_response();
        }
        doc.spaces.push(Space {
            id: id.clone(),
            name,
            tagline: req.tagline.trim().to_owned(),
            admins,
            projects: projects.clone(),
            members: members.clone(),
            budget_usd: req.budget_usd.clamp(0.0, 1_000_000.0),
            created_by: by,
            created_at: coxagent_application::state::now_rfc3339(),
        });
    }
    app.spaces.save().await;
    assign_space_members(&app, &members, &projects).await;
    Json(serde_json::json!({ "ok": true, "id": id })).into_response()
}

/// Update a space: super admin, or an admin OF that space.
async fn space_update_ep(
    State(app): State<AppState>,
    Path(sid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SpaceReq>,
) -> axum::response::Response {
    let sup = is_super(&app, &headers).await;
    let me = resolve_username(&app, &headers).await;
    let (admins, projects, members) = match validate_space_req(&app, &req, Some(sid.as_str())).await
    {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    {
        let mut doc = app.spaces.inner.lock().await;
        let Some(space) = doc.spaces.iter_mut().find(|s| s.id == sid) else {
            return not_found();
        };
        let allowed = sup || space.admins.iter().any(|a| a.eq_ignore_ascii_case(&me));
        if !allowed {
            return (StatusCode::FORBIDDEN, "not an admin of this space").into_response();
        }
        if !req.name.trim().is_empty() {
            req.name.trim().clone_into(&mut space.name);
        }
        req.tagline.trim().clone_into(&mut space.tagline);
        // Only the super admin reshapes membership/projects/budget of a space.
        if sup {
            space.admins = admins;
            space.projects = projects.clone();
            space.members = members.clone();
            space.budget_usd = req.budget_usd.clamp(0.0, 1_000_000.0);
        }
    }
    app.spaces.save().await;
    if sup {
        assign_space_members(&app, &members, &projects).await;
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

async fn space_delete_ep(
    State(app): State<AppState>,
    Path(sid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "super admin required").into_response();
    }
    {
        let mut doc = app.spaces.inner.lock().await;
        doc.spaces.retain(|s| s.id != sid);
    }
    app.spaces.save().await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// The super admin's cross-space overview: every space with its live stats
/// (projects, members, spend, online), plus hub totals and the user directory.
async fn manage_overview_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "super admin required").into_response();
    }
    let order = app.order.read().await.clone();
    let projects_map = app.projects.read().await.clone();
    // Per-project stats once, plus per-user spend rolled up across projects.
    let mut pstats: HashMap<String, (f64, usize, Vec<String>)> = HashMap::new();
    let mut user_spend: HashMap<String, f64> = HashMap::new();
    for pid in &order {
        let Some(p) = projects_map.get(pid) else {
            continue;
        };
        let mut spend = 0.0;
        if let Ok(st) = p.store.load().await {
            spend = st.spend.total_cost_usd;
            for (op, v) in &st.spend.by_operator {
                let user = op.split('@').next().unwrap_or(op).to_owned();
                *user_spend.entry(user).or_insert(0.0) += v.cost_usd;
            }
        }
        let workers = p.store.workers().await.unwrap_or_default();
        let online: Vec<String> = workers
            .iter()
            .map(|w| w.worker.split('@').next().unwrap_or("").to_owned())
            .collect();
        pstats.insert(pid.clone(), (spend, workers.len(), online));
    }
    let users = match app.auth.clone() {
        Some(auth) => auth.list_users().await,
        None => Vec::new(),
    };
    let spaces = app.spaces.inner.lock().await.spaces.clone();
    let mut assigned: std::collections::HashSet<String> = std::collections::HashSet::new();
    let spaces_json: Vec<serde_json::Value> = spaces
        .iter()
        .map(|s| {
            let mut spend = 0.0;
            let mut online: Vec<String> = Vec::new();
            for pid in &s.projects {
                assigned.insert(pid.clone());
                if let Some((sp, _, on)) = pstats.get(pid) {
                    spend += sp;
                    online.extend(on.clone());
                }
            }
            let member_list: Vec<_> = users
                .iter()
                .filter(|u| {
                    s.admins.iter().any(|a| a.eq_ignore_ascii_case(&u.username))
                        || s.members
                            .iter()
                            .any(|m| m.eq_ignore_ascii_case(&u.username))
                        || u.projects.iter().any(|p| s.projects.contains(p))
                })
                .collect();
            let mut roles: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for u in &member_list {
                *roles.entry(u.role.as_str().to_owned()).or_default() += 1;
            }
            serde_json::json!({
                "id": s.id, "name": s.name, "tagline": s.tagline,
                "admins": s.admins, "projects": s.projects,
                "members": member_list.len(), "roles": roles, "spend": spend,
                "budget_usd": s.budget_usd, "online": online,
            })
        })
        .collect();
    let unassigned: Vec<String> = order
        .iter()
        .filter(|p| !assigned.contains(*p))
        .cloned()
        .collect();
    Json(serde_json::json!({
        "spaces": spaces_json,
        "unassigned_projects": unassigned,
        "users": users.iter().map(|u| serde_json::json!({
            "username": u.username, "name": u.name, "role": u.role.as_str(),
            "projects": u.projects,
            "spend": user_spend.get(&u.username).copied().unwrap_or(0.0),
        })).collect::<Vec<_>>(),
        "totals": {
            "projects": order.len(),
            "users": users.len(),
            "spend": pstats.values().map(|(s,_,_)| s).sum::<f64>(),
            "online": pstats.values().map(|(_,n,_)| n).sum::<usize>(),
        },
    }))
    .into_response()
}

/// Deep-dive one space (super admin): every project's health/spend/sprint,
/// and every member with their role and burn — the drill-down behind a card.
async fn manage_space_detail_ep(
    State(app): State<AppState>,
    Path(sid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "super admin required").into_response();
    }
    let Some(space) = app
        .spaces
        .inner
        .lock()
        .await
        .spaces
        .iter()
        .find(|s| s.id == sid)
        .cloned()
    else {
        return not_found();
    };
    let projects_map = app.projects.read().await.clone();
    let mut projects = Vec::new();
    let mut user_spend: HashMap<String, f64> = HashMap::new();
    for pid in &space.projects {
        let Some(p) = projects_map.get(pid) else {
            continue;
        };
        let Ok(st) = p.store.load().await else {
            continue;
        };
        for (op, v) in &st.spend.by_operator {
            let user = op.split('@').next().unwrap_or(op).to_owned();
            *user_spend.entry(user).or_insert(0.0) += v.cost_usd;
        }
        let m = coxagent_application::metrics::compute(&st);
        let workers = p.store.workers().await.unwrap_or_default();
        projects.push(serde_json::json!({
            "id": p.id, "name": p.name, "version": m.version,
            "shipped": m.features_shipped, "in_flight": m.features_in_flight,
            "bugs_open": m.bugs_open, "total_tickets": m.total_tickets,
            "spend": st.spend.total_cost_usd,
            "tokens": st.spend.input_tokens + st.spend.output_tokens,
            "sprint": st.sprint.as_ref().map(|s| serde_json::json!({
                "number": s.number, "goal": s.goal,
                "done": coxagent_application::sprint::done_count(&st),
                "committed": s.committed.len(),
            })),
            "online": workers.iter().map(|w| w.worker.split('@').next().unwrap_or("").to_owned()).collect::<Vec<_>>(),
        }));
    }
    let users = match app.auth.clone() {
        Some(auth) => auth.list_users().await,
        None => Vec::new(),
    };
    let members: Vec<serde_json::Value> = users
        .iter()
        .filter(|u| {
            space
                .admins
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&u.username))
                || u.projects.iter().any(|p| space.projects.contains(p))
        })
        .map(|u| {
            serde_json::json!({
                "username": u.username, "name": u.name, "role": u.role.as_str(),
                "is_space_admin": space.admins.iter().any(|a| a.eq_ignore_ascii_case(&u.username)),
                "spend": user_spend.get(&u.username).copied().unwrap_or(0.0),
            })
        })
        .collect();
    Json(serde_json::json!({ "space": space, "projects": projects, "members": members }))
        .into_response()
}

/// Company-level overview: every project's health + spend + who's online, plus
/// the member directory — the workspace home screen's data.
async fn workspace_overview_ep(State(app): State<AppState>) -> axum::response::Response {
    let order = app.order.read().await.clone();
    let projects_map = app.projects.read().await.clone();
    let mut projects = Vec::new();
    for pid in &order {
        let Some(p) = projects_map.get(pid) else {
            continue;
        };
        let Ok(state) = p.store.load().await else {
            continue;
        };
        let m = coxagent_application::metrics::compute(&state);
        let workers = p.store.workers().await.unwrap_or_default();
        projects.push(serde_json::json!({
            "id": p.id, "name": p.name, "alias": state.alias,
            "version": m.version,
            "shipped": m.features_shipped, "in_flight": m.features_in_flight,
            "bugs_open": m.bugs_open, "total_tickets": m.total_tickets,
            "spend": state.spend.total_cost_usd,
            "sprint": state.sprint.as_ref().map(|s| serde_json::json!({"number": s.number, "goal": s.goal})),
            "online": workers.iter().map(|w| w.worker.split('@').next().unwrap_or("").to_owned()).collect::<Vec<_>>(),
        }));
    }
    let members = match app.auth.clone() {
        Some(auth) => auth
            .list_users()
            .await
            .into_iter()
            .map(|u| serde_json::json!({"username": u.username, "name": u.name, "role": u.role.as_str(), "projects": u.projects}))
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let w = app.workspace.inner.lock().await.clone();
    Json(serde_json::json!({
        "workspace": {"name": w.name, "tagline": w.tagline, "accent": w.accent},
        "projects": projects, "members": members,
    }))
    .into_response()
}

/// The signed-in user's agents across every project they belong to: online
/// state, current work, desired flag, and their token spend — the "my agents"
/// management panel.
async fn my_agents_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let me = resolve_username(&app, &headers).await;
    let (is_admin, my_projects) = match app.auth.clone() {
        Some(auth) => resolve_principal(&auth, &headers)
            .await
            .map_or((false, Vec::new()), |u| (u.role.can_manage(), u.projects)),
        None => (true, Vec::new()),
    };
    let order = app.order.read().await.clone();
    let projects_map = app.projects.read().await.clone();
    let prefix = format!("{me}@");
    let mut out = Vec::new();
    for pid in &order {
        let Some(p) = projects_map.get(pid) else {
            continue;
        };
        if !is_admin && !my_projects.contains(pid) {
            continue;
        }
        let workers = p.store.workers().await.unwrap_or_default();
        let mine: Vec<_> = workers
            .iter()
            .filter(|w| w.worker.starts_with(&prefix))
            .collect();
        let spend = p.store.load().await.ok().map(|s| {
            s.spend
                .by_operator
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(_, v)| (v.cost_usd, v.input_tokens + v.output_tokens))
                .fold((0.0, 0u64), |a, b| (a.0 + b.0, a.1 + b.1))
        });
        let (cost, tokens) = spend.unwrap_or((0.0, 0));
        let operator = mine.first().map(|w| w.worker.clone());
        let desired = match &operator {
            Some(op) => p.store.get_desired(op).await.ok().flatten(),
            None => None,
        };
        out.push(serde_json::json!({
            "project": pid, "name": p.name,
            "online": !mine.is_empty(),
            "operator": operator,
            "role": mine.first().map(|w| w.role.clone()),
            "ticket": mine.first().map(|w| w.ticket.clone()),
            "desired": desired,
            "cost": cost, "tokens": tokens,
        }));
    }
    Json(serde_json::json!({ "username": me, "agents": out })).into_response()
}

/// Minimal HTML escaping for the join page.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// On-demand SA merge sweep: merge every green PR in the queue right now
/// (oldest first), report to `#agents`, and return the outcome. Token-free.
async fn merge_sweep_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(forge) = &p.forge else {
        return (StatusCode::NOT_IMPLEMENTED, "forge not configured").into_response();
    };
    // Target branch + ceremony language from the project config file.
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_default();
    let target = cfg["git"]["target_branch"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| cfg["git"]["default_branch"].as_str())
        .unwrap_or("main")
        .to_owned();
    let vi = cfg["workflow"]["language"].as_str() == Some("vi");
    let require_ci = cfg["git"]["require_ci"].as_bool().unwrap_or(true);
    let out = coxagent_application::use_cases::merge_sweep(
        forge.as_ref(),
        p.store.as_ref(),
        &target,
        vi,
        require_ci,
    )
    .await;
    Json(serde_json::json!({ "ok": true, "merged": out.merged, "skipped": out.skipped }))
        .into_response()
}

#[derive(serde::Deserialize)]
struct SprintGoalReq {
    goal: String,
}

/// Set the PO's goal for the upcoming sprint. It becomes the sprint goal on the
/// next roll-over and steers the BA to propose tickets that advance it — the
/// proposals still pass the normal Pending→Ready refinement gate before any DEV
/// work, so nothing is built without vetting.
async fn set_sprint_goal_ep(
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
        let online = guard.online_users();
        let handle = handle.clone();
        async move {
            let payload = match handle {
                Some(p) => serde_json::json!({
                    "state": p.store.load().await.ok().as_ref().map(lite_state_value),
                    "runner": p.runner.snapshot(),
                    "viewers": count,
                    "online": online,
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
#[allow(clippy::too_many_lines)] // linear gate list; splitting hides the order
async fn auth_mw(
    State(app): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let path = req.uri().path().to_owned();
    // Physical service split: a realtime pod serves only sockets, a knowledge
    // pod serves nothing but health — enforced here, not by routing hope.
    // (Realtime endpoints carry their own guard; this blocks the REST surface.)
    let realtime_path = path.ends_with("/ws")
        || path.ends_with("/events")
        || path.ends_with("/terminal")
        || path.contains("/docs-ws");
    let role_ok = match hub_role() {
        HubRole::All => true,
        HubRole::Gateway => !realtime_path,
        HubRole::Realtime => {
            realtime_path || path == "/api/health" || path == "/api/auth/me" || path == "/"
        }
        HubRole::Knowledge => path == "/api/health",
    };
    if !role_ok {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "wrong service role for this endpoint",
        )
            .into_response();
    }
    let Some(auth) = app.auth.clone() else {
        return next.run(req).await;
    };
    // Public routes: the SPA shell, health, login, and incoming webhooks (the
    // webhook token is the credential, so no session is required).
    if path == "/"
        || path == "/api/health"
        || path == "/api/auth/login"
        // Embedded static assets (vendored JS/CSS) — same trust level as "/".
        || path.starts_with("/assets/")
        // Installer downloads: same trust as the login page; the native
        // updater's URLSession has no web session to present.
        || path.starts_with("/api/app/download/")
        || path.starts_with("/api/chat/hook/")
        // Invite flow: the invite token IS the credential for joining.
        || path.starts_with("/join/")
        || path == "/api/workspace/join"
    {
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
        && !path.starts_with("/api/auth/my/") // personal MCP tokens: self-service
        && !path.starts_with("/api/meetings") // booking meetings: any signed-in user
        && !path.starts_with("/api/profile") // own avatar/status: self-service
        && !path.ends_with("/chat") // team chat is open to any signed-in user
        && path != "/api/chat/send" // system chat send: any signed-in user
        && path != "/api/chat/dm" // open a DM: any signed-in user
        && !path.starts_with("/api/engines/opencode") // opencode model list: any signed-in user
        && !path.contains("/channels") // create/invite channels: any signed-in user
        && !path.ends_with("/upload"); // uploads are open to any signed-in user
    let method = req.method().clone();
    let username = user.username.clone();
    // Management surfaces — user administration, project Settings, and API
    // tokens — are limited to Admin + lead tier (Director/Manager/*.Lead).
    // Member-tier roles (BA/FE/BE/…) can work and chat but not administer.
    let is_manage_surface = path.starts_with("/api/auth/users")
        || path.starts_with("/api/auth/tokens")
        || (path.ends_with("/config")
            && matches!(
                *req.method(),
                axum::http::Method::PUT | axum::http::Method::POST | axum::http::Method::PATCH
            ));
    if is_manage_surface && !user.role.can_manage() {
        audit_push(
            &app.audit,
            &username,
            format!("{method} {path}"),
            StatusCode::FORBIDDEN.as_u16(),
        )
        .await;
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "management role required" })),
        )
            .into_response();
    }
    // Per-project access control: extract pid from URL path and verify the
    // user is assigned to that project (Super/Admin bypass, members checked).
    if let Some(pid) = extract_pid_from_path(&path) {
        let is_super_or_admin = user.role == coxagent_application::auth::AuthRole::Super
            || user.role == coxagent_application::auth::AuthRole::Admin;
        if !is_super_or_admin && !user.projects.iter().any(|p| p == pid) {
            audit_push(&app.audit, &username, format!("{method} {path}"), 403).await;
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "not a member of this project" })),
            )
                .into_response();
        }
    }
    // PR review actions (merge / request-changes / close) are allowed for
    // reviewers as well as admins; every other write stays admin-only.
    let is_review_action = path.contains("/prs/");
    let write_ok = if is_review_action {
        user.role.can_review()
    } else {
        user.role.can_write()
    };
    if is_write && !write_ok {
        audit_push(
            &app.audit,
            &username,
            format!("{method} {path}"),
            StatusCode::FORBIDDEN.as_u16(),
        )
        .await;
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "insufficient role" })),
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
    let role = coxagent_application::AuthRole::from_str_lenient(req.role.as_deref().unwrap_or(""));
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
        .is_some_and(|u| u.role.can_manage());
    if !is_admin {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    // internal:mcp:* tokens are plumbing minted for an operator's spawned
    // agent CLIs to call this hub's own /api/mcp (see app::ensure_internal_mcp_token)
    // — not a human-managed credential, so keep them out of the admin list.
    let tokens: Vec<_> = auth
        .list_tokens()
        .await
        .into_iter()
        .filter(|t| !t.label.starts_with("internal:mcp:"))
        .collect();
    Json(tokens).into_response()
}

/// Prefix that namespaces a user's personal (self-service) tokens. Personal
/// tokens are minted at the caller's OWN role — never an elevation — and are
/// the credential the Settings → MCP tab hands to MCP clients.
fn personal_token_prefix(username: &str) -> String {
    format!("user:{}:", username.to_ascii_lowercase())
}

/// List the caller's own personal tokens (metadata only). Any signed-in user.
async fn my_tokens_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(Vec::<coxagent_application::TokenInfo>::new()).into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let prefix = personal_token_prefix(&user.username);
    let mine: Vec<_> = auth
        .list_tokens()
        .await
        .into_iter()
        .filter(|t| t.label.starts_with(&prefix))
        .collect();
    Json(mine).into_response()
}

#[derive(serde::Deserialize)]
struct CreateMyTokenReq {
    label: String,
}

/// Mint a personal API token bound to the caller's own account and role.
/// Any signed-in user; the secret is returned once and never stored.
async fn create_my_token_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateMyTokenReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let short = req
        .label
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(40)
        .collect::<String>();
    if short.is_empty() {
        return (StatusCode::BAD_REQUEST, "label is required").into_response();
    }
    let label = format!("{}{short}", personal_token_prefix(&user.username));
    match auth.create_token(&label, user.role).await {
        Some(secret) => {
            audit_push(
                &app.audit,
                &user.username,
                format!("personal token minted: {label}"),
                200,
            )
            .await;
            Json(serde_json::json!({
                "ok": true, "label": label, "token": secret,
                "note": "store this now — it is not shown again",
            }))
            .into_response()
        }
        None => (StatusCode::CONFLICT, "label already in use").into_response(),
    }
}

/// Revoke one of the caller's OWN personal tokens. Any signed-in user; the
/// prefix check makes it impossible to revoke another account's token.
async fn revoke_my_token_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(label): Path<String>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    if !label.starts_with(&personal_token_prefix(&user.username)) {
        return (StatusCode::FORBIDDEN, "not your token").into_response();
    }
    if auth.revoke_token(&label).await {
        audit_push(
            &app.audit,
            &user.username,
            format!("personal token revoked: {label}"),
            200,
        )
        .await;
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such token").into_response()
    }
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
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    /// Project ids to assign the new user to (a user may join many).
    #[serde(default)]
    projects: Vec<String>,
}

fn role_from(s: Option<&str>) -> coxagent_application::AuthRole {
    coxagent_application::AuthRole::from_str_lenient(s.unwrap_or(""))
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
        .is_some_and(|u| u.role.can_manage());
    if !is_admin {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    Json(auth.list_users().await).into_response()
}

/// Create or update a user account (admin-only via the write gate).
async fn create_user_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateUserReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    // Only admins and leads can create user accounts.
    let is_admin = resolve_principal(&auth, &headers)
        .await
        .is_some_and(|u| u.role.can_manage());
    if !is_admin {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    if req.username.trim().is_empty() || req.password.is_empty() {
        return (StatusCode::BAD_REQUEST, "username and password required").into_response();
    }
    let username = req.username.trim();
    let role = role_from(req.role.as_deref());
    if !auth.create_user(username, &req.password, role).await {
        return internal_error("could not create user");
    }
    // Best-effort profile + project assignment on the freshly created account.
    if !req.name.trim().is_empty() || !req.email.trim().is_empty() {
        auth.update_user(username, req.name.trim(), req.email.trim(), None)
            .await;
    }
    for pid in &req.projects {
        auth.assign_project(username, pid).await;
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[derive(serde::Deserialize)]
struct UpdateUserReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    role: Option<String>,
}

/// Update a user's profile (name/email) and optionally role. Admin-only.
async fn update_user_ep(
    State(app): State<AppState>,
    Path(username): Path<String>,
    Json(req): Json<UpdateUserReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let role = req.role.as_deref().map(|s| role_from(Some(s)));
    if auth
        .update_user(&username, req.name.trim(), req.email.trim(), role)
        .await
    {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such user").into_response()
    }
}

#[derive(serde::Deserialize)]
struct SelfProfileReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: Option<String>,
}

/// Self-service account update: the signed-in user edits their OWN display
/// name / email / password. Never role — that stays admin-only.
async fn self_profile_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SelfProfileReq>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if !auth
        .update_user(&user, req.name.trim(), req.email.trim(), None)
        .await
    {
        return (StatusCode::NOT_FOUND, "no such user").into_response();
    }
    if let Some(pw) = req.password.as_deref().filter(|p| !p.is_empty()) {
        if pw.len() < 8 {
            return (StatusCode::BAD_REQUEST, "password too short (min 8)").into_response();
        }
        if !auth.set_password(&user, pw).await {
            return (StatusCode::NOT_FOUND, "no such user").into_response();
        }
    }
    audit_push(&app.audit, &user, "own profile updated".to_owned(), 200).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[derive(serde::Deserialize)]
struct ResetPasswordReq {
    password: String,
}

/// Reset a user's password. Admin-only.
async fn reset_password_ep(
    State(app): State<AppState>,
    Path(username): Path<String>,
    Json(req): Json<ResetPasswordReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if req.password.len() < 8 {
        return (StatusCode::BAD_REQUEST, "password too short").into_response();
    }
    if auth.set_password(&username, &req.password).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such user").into_response()
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
    // Only admins/super can see the full member list; regular members see only
    // users assigned to their project (filtered by project membership).
    let user = resolve_principal(&auth, &headers).await;
    let Some(user) = user else {
        return (StatusCode::FORBIDDEN, "sign-in required").into_response();
    };
    let is_privileged = user.role.can_manage();
    let out: Vec<serde_json::Value> = auth
        .list_users()
        .await
        .into_iter()
        .filter(|u| is_privileged || u.projects.iter().any(|p| p.as_str() == pid.as_str()))
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
    // Hub-wide audit trail: Admin/Super only — it records everyone's actions,
    // so ordinary members (can_write) must NOT read it.
    if let Some(auth) = app.auth.clone() {
        let ok = match resolve_principal(&auth, &headers).await {
            Some(u) => matches!(
                u.role,
                coxagent_application::auth::AuthRole::Admin
                    | coxagent_application::auth::AuthRole::Super
            ),
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
    let role = user.as_ref().map_or("viewer", |u| u.role.as_str());
    audit_push(&app.audit, &req.username, "login".to_owned(), 200).await;
    let cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200{}",
        cookie_secure(&headers)
    );
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
    let cleared = format!(
        "{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{}",
        cookie_secure(&headers)
    );
    (
        [(header::SET_COOKIE, cleared)],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

/// The `; Secure` cookie attribute when the connection is TLS-terminated —
/// detected via `X-Forwarded-Proto: https` (behind a reverse proxy) or the
/// `COXAGENT_SECURE_COOKIES=1` opt-in. Omitted for plain-HTTP localhost so the
/// cookie still works there.
fn cookie_secure(headers: &axum::http::HeaderMap) -> &'static str {
    let forwarded_https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p.eq_ignore_ascii_case("https"));
    let forced = std::env::var("COXAGENT_SECURE_COOKIES")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    if forwarded_https || forced {
        "; Secure"
    } else {
        ""
    }
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
            // Session principals are login-time snapshots; read the CURRENT
            // record so a just-saved display name/email shows immediately.
            let fresh = auth
                .list_users()
                .await
                .into_iter()
                .find(|x| x.username == u.username);
            let (name, email) =
                fresh.map_or((u.name.clone(), u.email.clone()), |f| (f.name, f.email));
            Json(serde_json::json!({
                "auth": true, "username": u.username,
                "name": name, "email": email,
                "role": u.role.as_str(),
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

#[cfg(test)]
mod pr_preview_tests {
    use super::*;
    use coxagent_application::config::BudgetCaps;
    use coxagent_application::ports::outbound::{
        AgentOutcome, AgentRequest, DeployPort, DeployReport, ForgePort, PullRequest,
    };
    use coxagent_application::state::ProjectState;
    use coxagent_application::PortError;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }
    #[async_trait::async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
            *self.state.lock().expect("lock") = s.clone();
            Ok(())
        }
    }

    /// Never invoked by the restore (`start=false`) path under test.
    struct UnusedEngine;
    #[async_trait::async_trait]
    impl coxagent_application::ports::outbound::AgentEnginePort for UnusedEngine {
        fn id(&self) -> &'static str {
            "unused"
        }
        async fn run(&self, _request: AgentRequest) -> Result<AgentOutcome, PortError> {
            unreachable!("not called by pr_preview's restore path")
        }
    }

    /// Never invoked by the restore (`start=false`) path under test — it does
    /// no PR/git lookups, only `deploy.down` + `deploy.deploy`.
    struct UnusedForge;
    #[async_trait::async_trait]
    impl ForgePort for UnusedForge {
        async fn open_pr(
            &self,
            _head: &str,
            _base: &str,
            _title: &str,
            _body: &str,
        ) -> Result<PullRequest, PortError> {
            unreachable!("not called by pr_preview's restore path")
        }
        async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
            unreachable!("not called by pr_preview's restore path")
        }
        async fn pr_diff(&self, _number: u64) -> Result<String, PortError> {
            unreachable!("not called by pr_preview's restore path")
        }
        async fn merge_pr(&self, _number: u64) -> Result<(), PortError> {
            unreachable!("not called by pr_preview's restore path")
        }
        async fn request_changes(&self, _number: u64, _comment: &str) -> Result<(), PortError> {
            unreachable!("not called by pr_preview's restore path")
        }
        async fn close_pr(&self, _number: u64) -> Result<(), PortError> {
            unreachable!("not called by pr_preview's restore path")
        }
    }

    /// `docker compose up` exits 0 (container started) but the app inside
    /// never answers on its configured port — the COX-B004/COX-B009 scenario.
    struct DeployWithDeadPort;
    #[async_trait::async_trait]
    impl DeployPort for DeployWithDeadPort {
        async fn deploy(&self, _work_dir: &std::path::Path) -> Result<DeployReport, PortError> {
            Ok(DeployReport {
                success: true,
                deployed: true,
                summary: "docker compose up -d --build succeeded".to_owned(),
            })
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            Ok(false)
        }
    }

    struct HealthyDeploy;
    #[async_trait::async_trait]
    impl DeployPort for HealthyDeploy {
        async fn deploy(&self, _work_dir: &std::path::Path) -> Result<DeployReport, PortError> {
            Ok(DeployReport {
                success: true,
                deployed: true,
                summary: "docker compose up -d --build succeeded".to_owned(),
            })
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            Ok(true)
        }
    }

    /// A deploy that must never be reached: a config whose health can't be
    /// verified has to be refused *before* anything is deployed, not deployed
    /// first and judged after.
    struct NeverDeploys;
    #[async_trait::async_trait]
    impl DeployPort for NeverDeploys {
        async fn deploy(&self, _work_dir: &std::path::Path) -> Result<DeployReport, PortError> {
            unreachable!("an unverifiable host_port must be refused before any deploy")
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            unreachable!("an unverifiable host_port must be refused before any probe")
        }
    }

    /// A project workspace with a `coxagent.json` naming the published
    /// `deploy.host_port` `pr_preview` reads to probe health.
    fn project_handle(deploy: Arc<dyn DeployPort>) -> (tempfile::TempDir, ProjectHandle) {
        project_handle_with_port(deploy, "8101")
    }

    /// [`project_handle`] with an arbitrary `deploy.host_port` JSON literal, so
    /// a test can hand `pr_preview` a port its typed config would never hold.
    fn project_handle_with_port(
        deploy: Arc<dyn DeployPort>,
        host_port: &str,
    ) -> (tempfile::TempDir, ProjectHandle) {
        project_handle_with_config(
            deploy,
            &format!(r#"{{"deploy":{{"host_port":{host_port}}}}}"#),
        )
    }

    /// [`project_handle`] over a verbatim `coxagent.json` body, for the cases
    /// `deploy.host_port` isn't a value at all — an absent key, an absent
    /// `deploy` block.
    fn project_handle_with_config(
        deploy: Arc<dyn DeployPort>,
        config: &str,
    ) -> (tempfile::TempDir, ProjectHandle) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("coxagent.json");
        std::fs::write(&config_path, config).expect("write config");
        let handle = ProjectHandle {
            id: "proj".to_owned(),
            name: "proj".to_owned(),
            alias: "proj".to_owned(),
            store: Arc::new(MemStore::default()) as Arc<dyn StateStorePort>,
            runner: Arc::new(RunnerHandle::default()),
            config_path,
            engine: Arc::new(UnusedEngine),
            work_dir: dir.path().to_path_buf(),
            budget: Arc::new(Mutex::new(BudgetCaps::default())),
            context_path: dir.path().join("project_context.md"),
            forge: None,
            deploy: Some(deploy),
        };
        (dir, handle)
    }

    /// AC (COX-B009): restoring the main build after a PR preview must run
    /// through the same mandatory health gate as the autonomous cycle
    /// (COX-B004) and chat's "deploy" command — a compose exit-0 that never
    /// binds the app's port must NOT be reported as a successful restore.
    #[tokio::test(start_paused = true)]
    async fn restore_reports_failure_when_the_app_never_binds_its_port() {
        let (_dir, handle) = project_handle(Arc::new(DeployWithDeadPort));
        let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

        let resp = pr_preview(&handle, &forge, 1, false).await;

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("health check failed"),
            "expected the health-gate failure reason in the response: {text}"
        );
    }

    /// Control: a restore that actually answers on its port still reports OK
    /// — the gate must not fail a genuinely healthy restore.
    #[tokio::test(start_paused = true)]
    async fn restore_reports_ok_when_the_app_is_healthy() {
        let (_dir, handle) = project_handle(Arc::new(HealthyDeploy));
        let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

        let resp = pr_preview(&handle, &forge, 1, false).await;

        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// AC (COX-B009): the gate must not be skippable. A `host_port` that isn't
    /// a TCP port leaves nothing to probe, so reporting success would hand
    /// back the exact unverified "LIVE" the gate exists to prevent — the
    /// deploy is rejected instead, even though the app reports itself healthy.
    #[tokio::test(start_paused = true)]
    async fn an_out_of_range_host_port_is_rejected_rather_than_skipping_the_gate() {
        let (_dir, handle) = project_handle_with_port(Arc::new(HealthyDeploy), "70000");
        let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

        let resp = pr_preview(&handle, &forge, 1, false).await;

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("not a valid TCP port"),
            "expected the invalid-port reason in the response: {text}"
        );
    }

    /// Every `deploy.host_port` shape the typed config (`Option<u16>`) would
    /// refuse to load, as an on-disk `coxagent.json` can still hold it.
    const MALFORMED_HOST_PORTS: [&str; 6] = [r#""8101""#, "-1", "8101.5", "true", "{}", "70000"];

    /// AC (COX-B025): "invalid config" and "no config" are distinct, and only
    /// the second one means there's nothing to probe. Reading the port out of
    /// raw JSON used to collapse both into `None` — every malformed value
    /// below skipped the mandatory health gate and reported a false "LIVE".
    /// Each is refused up front instead, and refused before `deploy()` runs,
    /// so an unverifiable config never reaches a deploy at all.
    #[tokio::test(start_paused = true)]
    async fn a_malformed_host_port_is_rejected_rather_than_skipping_the_gate() {
        for host_port in MALFORMED_HOST_PORTS {
            let (_dir, handle) = project_handle_with_port(Arc::new(NeverDeploys), host_port);
            let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

            let resp = pr_preview(&handle, &forge, 1, false).await;

            assert_eq!(
                resp.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "host_port {host_port} should be refused"
            );
            let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .expect("body");
            let text = String::from_utf8_lossy(&body);
            assert!(
                text.contains("not a valid TCP port"),
                "expected the invalid-port reason for host_port {host_port}: {text}"
            );
        }
    }

    /// AC (COX-B025): the start path is gated by the same check, and reaches
    /// it before it resolves the PR — a preview whose health can't be verified
    /// is refused, not started. [`UnusedForge`] panics if it's consulted.
    #[tokio::test(start_paused = true)]
    async fn a_malformed_host_port_is_rejected_before_a_preview_starts() {
        for host_port in MALFORMED_HOST_PORTS {
            let (_dir, handle) = project_handle_with_port(Arc::new(NeverDeploys), host_port);
            let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

            let resp = pr_preview(&handle, &forge, 1, true).await;

            assert_eq!(
                resp.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "host_port {host_port} should be refused"
            );
        }
    }

    /// The other half of the distinction: a project that genuinely publishes
    /// no port has nothing to probe, so the gate passes vacuously exactly as
    /// [`verify_deploy_health`] documents. Tightening the invalid-port cases
    /// must not turn "unset" into a failure.
    ///
    /// Covers the *config shapes* that mean "no port" — an explicit null, an
    /// empty deploy block, an empty document, and an unparseable one. The
    /// start-path counterpart is
    /// `an_unset_host_port_leaves_the_gate_nothing_to_probe`.
    #[tokio::test(start_paused = true)]
    async fn no_host_port_in_any_config_shape_leaves_the_gate_nothing_to_probe() {
        for config in [
            r#"{"deploy":{"host_port":null}}"#,
            r#"{"deploy":{}}"#,
            "{}",
            "not json at all",
        ] {
            let (_dir, handle) = project_handle_with_config(Arc::new(HealthyDeploy), config);
            let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

            let resp = pr_preview(&handle, &forge, 1, false).await;

            assert_eq!(resp.status(), StatusCode::OK, "config {config}");
        }
    }

    /// A `ForgePort` stub reporting a single open PR whose head branch is
    /// fetchable from the fixture's `origin` remote.
    struct ForgeWithOpenPr(String);
    #[async_trait::async_trait]
    impl ForgePort for ForgeWithOpenPr {
        async fn open_pr(
            &self,
            _head: &str,
            _base: &str,
            _title: &str,
            _body: &str,
        ) -> Result<PullRequest, PortError> {
            unreachable!("not called by pr_preview's start path")
        }
        async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
            Ok(vec![PullRequest {
                number: 1,
                title: "test".to_owned(),
                head: self.0.clone(),
                base: "main".to_owned(),
                url: String::new(),
                author: "tester".to_owned(),
                ci: "none".to_owned(),
                mergeable: true,
                created: "2026-01-01T00:00:00Z".to_owned(),
            }])
        }
        async fn pr_diff(&self, _number: u64) -> Result<String, PortError> {
            unreachable!("not called by pr_preview's start path")
        }
        async fn merge_pr(&self, _number: u64) -> Result<(), PortError> {
            unreachable!("not called by pr_preview's start path")
        }
        async fn request_changes(&self, _number: u64, _comment: &str) -> Result<(), PortError> {
            unreachable!("not called by pr_preview's start path")
        }
        async fn close_pr(&self, _number: u64) -> Result<(), PortError> {
            unreachable!("not called by pr_preview's start path")
        }
    }

    /// A bare `origin` repo with a `feat/preview` branch, plus a working
    /// clone wired as the project's `work_dir` — real git, since the start
    /// path shells out to `git fetch`/`git worktree add`.
    async fn git_preview_fixture(
        deploy: Arc<dyn DeployPort>,
        host_port: Option<u64>,
    ) -> (tempfile::TempDir, tempfile::TempDir, ProjectHandle) {
        let bare = tempfile::tempdir().expect("tempdir");
        git_pv(bare.path(), &["init", "--bare", "-q"])
            .await
            .expect("git init --bare");
        let bare_url = bare.path().to_string_lossy().into_owned();

        let seed = tempfile::tempdir().expect("tempdir");
        git_pv(seed.path(), &["init", "-q", "-b", "main"])
            .await
            .expect("git init seed");
        git_pv(seed.path(), &["config", "user.email", "test@test"])
            .await
            .expect("git config email");
        git_pv(seed.path(), &["config", "user.name", "test"])
            .await
            .expect("git config name");
        std::fs::write(seed.path().join("README.md"), "seed").expect("write");
        git_pv(seed.path(), &["add", "."]).await.expect("git add");
        git_pv(seed.path(), &["commit", "-q", "-m", "seed"])
            .await
            .expect("git commit");
        git_pv(seed.path(), &["checkout", "-q", "-b", "feat/preview"])
            .await
            .expect("git checkout -b");
        std::fs::write(seed.path().join("README.md"), "preview").expect("write");
        git_pv(seed.path(), &["commit", "-q", "-am", "preview change"])
            .await
            .expect("git commit");
        git_pv(seed.path(), &["remote", "add", "origin", &bare_url])
            .await
            .expect("remote add");
        git_pv(seed.path(), &["push", "-q", "origin", "--all"])
            .await
            .expect("git push");

        let work = tempfile::tempdir().expect("tempdir");
        git_pv(work.path(), &["clone", "-q", &bare_url, "."])
            .await
            .expect("git clone");
        let config_path = work.path().join("coxagent.json");
        let config = match host_port {
            Some(port) => format!(r#"{{"deploy":{{"host_port":{port}}}}}"#),
            None => r#"{"deploy":{}}"#.to_owned(),
        };
        std::fs::write(&config_path, config).expect("write config");
        let handle = ProjectHandle {
            id: "proj".to_owned(),
            name: "proj".to_owned(),
            alias: "proj".to_owned(),
            store: Arc::new(MemStore::default()) as Arc<dyn StateStorePort>,
            runner: Arc::new(RunnerHandle::default()),
            config_path,
            engine: Arc::new(UnusedEngine),
            work_dir: work.path().to_path_buf(),
            budget: Arc::new(Mutex::new(BudgetCaps::default())),
            context_path: work.path().join("project_context.md"),
            forge: None,
            deploy: Some(deploy),
        };
        (bare, work, handle)
    }

    /// AC (COX-B009): starting a PR preview must run through the same
    /// mandatory health gate as restore/chat/cycle — a compose exit-0 that
    /// never binds the app's port must NOT be reported as a LIVE preview.
    #[tokio::test(start_paused = true)]
    async fn preview_start_reports_failure_when_the_app_never_binds_its_port() {
        let (_bare, _work, handle) =
            git_preview_fixture(Arc::new(DeployWithDeadPort), Some(8101)).await;
        let forge: Arc<dyn ForgePort> = Arc::new(ForgeWithOpenPr("feat/preview".to_owned()));

        let resp = pr_preview(&handle, &forge, 1, true).await;

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("health check failed"),
            "expected the health-gate failure reason in the response: {text}"
        );
    }

    /// Control: a preview that actually answers on its port reports LIVE —
    /// the gate must not fail a genuinely healthy preview.
    #[tokio::test(start_paused = true)]
    async fn preview_start_reports_ok_when_the_app_is_healthy() {
        let (_bare, _work, handle) = git_preview_fixture(Arc::new(HealthyDeploy), Some(8101)).await;
        let forge: Arc<dyn ForgePort> = Arc::new(ForgeWithOpenPr("feat/preview".to_owned()));

        let resp = pr_preview(&handle, &forge, 1, true).await;

        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// AC (COX-B009): a project with no configured `deploy.host_port` has
    /// nothing for the gate to probe — same contract as
    /// `verify_deploy_health`'s own `no_configured_host_port_passes_without_probing`
    /// unit test, exercised here through the actual preview endpoint. Uses a
    /// deploy adapter that would fail any real probe, so a false pass here
    /// would mean the gate is probing a port that was never configured.
    #[tokio::test(start_paused = true)]
    async fn an_unset_host_port_leaves_the_gate_nothing_to_probe() {
        let (_bare, _work, handle) = git_preview_fixture(Arc::new(DeployWithDeadPort), None).await;
        let forge: Arc<dyn ForgePort> = Arc::new(ForgeWithOpenPr("feat/preview".to_owned()));

        let resp = pr_preview(&handle, &forge, 1, true).await;

        assert_eq!(resp.status(), StatusCode::OK);
    }
}
