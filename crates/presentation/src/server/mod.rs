//! HTTP server (inbound adapter) — a multi-project hub. Serves the embedded
//! dashboard plus a per-project JSON API, SSE stream, controllable runner, and
//! ticket actions. A single-project `serve` registers one project; `hub`
//! registers many. Routes are scoped `/api/projects/:pid/...`.

#![allow(clippy::wildcard_imports)] // see server/{auth,chat,meetings}.rs — one module, many files
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

mod assets;
mod auth;
mod background;
mod channels;
mod chat;
mod comments;
mod docs;
mod engines;
mod forge;
mod manage;
mod meetings;
mod people;
mod projects;
mod transcripts;
mod realtime;
mod status;
mod work;

use assets::*;
use auth::*;
use background::*;
use channels::*;
use chat::*;
use comments::*;
use docs::*;
use engines::*;
use forge::*;
use manage::*;
use people::*;
use transcripts::*;
use meetings::*;
use projects::*;
use realtime::*;
use status::*;
use work::*;

/// The embedded single-page dashboard.
const INDEX_HTML: &str = include_str!("../web/index.html");
// Vendored terminal assets — embedded so the terminal works offline/air-gapped.
const XTERM_JS: &str = include_str!("../web/xterm.min.js");
const XTERM_CSS: &str = include_str!("../web/xterm.min.css");
const XTERM_FIT_JS: &str = include_str!("../web/xterm-addon-fit.min.js");

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
    /// Workspace file access for on-demand reviews; injected by the
    /// composition root so this layer stays free of infrastructure.
    pub files: Option<Arc<dyn coxagent_application::ports::outbound::WorkspaceFilesPort>>,
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
#[allow(clippy::struct_excessive_bools)] // a persisted data aggregate, not a state machine
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

fn parse_rfc3339(s: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
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

#[derive(serde::Deserialize)]
struct MeetingRingReq {
    user: String,
}

/// The caller's username, via session cookie or bearer token.
async fn principal_name(app: &AppState, headers: &axum::http::HeaderMap) -> Option<String> {
    let auth = app.auth.clone()?;
    resolve_principal(&auth, headers).await.map(|u| u.username)
}

#[allow(clippy::cast_possible_truncation)] // the low 32 bits of the hash IS the value
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

/// Warn threshold for a space's budget, matching the dashboard's own amber one
/// (index.html renders the "nearly reached" alert at 80% of a project's cap) —
/// same UX language, just at the space level and pushed as a chat heads-up.
const WARN_PCT: f64 = 0.8;

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

/// Compose-project prefix of a PR preview (`<workspace>/.preview/<num>` via
/// `compose_project_name`). Previews are meant to live for as long as someone
/// is looking at them.
const PREVIEW_PROJECT_PREFIX: &str = "cox--preview-";
/// How long a PR preview may stay up before the janitor reclaims it. Someone
/// opens a preview, reads the diff, and walks away — without this, the
/// container holds the app port and its share of the host for good. (One was
/// found still running after eight days.)
const PREVIEW_TTL: &str = "6h";

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
            "/api/chat/channels/:cid/settings",
            axum::routing::patch(syschat_settings_ep),
        )
        .route(
            "/api/chat/channels/:cid/members/:member",
            delete(syschat_kick_ep),
        )
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
            "/api/projects/:pid/channels/:cid/settings",
            axum::routing::patch(channel_settings_ep),
        )
        .route(
            "/api/projects/:pid/channels/:cid/members/:member",
            delete(channel_kick_ep),
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

#[derive(serde::Deserialize)]
struct ConnectReq {
    token: String,
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

#[derive(serde::Deserialize)]
struct RenameProjectReq {
    name: String,
}

#[derive(serde::Deserialize)]
struct GoalReq {
    goal: String,
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

#[derive(serde::Deserialize)]
struct AnalyzeReq {
    description: String,
}

#[derive(serde::Deserialize)]
struct EditReq {
    title: String,
    #[serde(default)]
    description: String,
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

#[derive(serde::Deserialize)]
struct ChatListQuery {
    /// Which channel's history to return; defaults to `#general`.
    channel: Option<String>,
}

#[derive(serde::Deserialize)]
struct CreateChannelReq {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    /// Open this channel INSIDE another one (the `+` on a channel row).
    #[serde(default)]
    parent: Option<String>,
}

#[derive(serde::Deserialize)]
struct ChannelSettingsReq {
    /// `"private"` or `"public"`. `#general` may not change.
    #[serde(default)]
    kind: Option<String>,
    /// Whether any member may invite; false leaves it to the owner and the
    /// people they delegated to.
    #[serde(default)]
    open_invite: Option<bool>,
    #[serde(default)]
    topic: Option<String>,
}

/// Whether the caller holds admin/super authority, which outranks channel
/// ownership everywhere it is checked.
async fn user_can_manage(app: &AppState, headers: &axum::http::HeaderMap) -> bool {
    let Some(auth) = app.auth.clone() else {
        return true; // running open (no auth configured)
    };
    resolve_principal(&auth, headers)
        .await
        .is_some_and(|u| u.role.can_manage())
}

#[derive(serde::Deserialize)]
struct ChannelMemberReq {
    /// Username to invite or delegate to.
    user: String,
    /// When true, grant invite permission (owner only), not just membership.
    #[serde(default)]
    delegate: bool,
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
struct DocUpsertReq {
    #[serde(default)]
    folder: String,
    title: String,
    #[serde(default)]
    body: String,
}

#[derive(serde::Deserialize)]
struct DocEditReq {
    instruction: String,
}

#[derive(serde::Deserialize)]
struct FolderReq {
    path: String,
}

/// Serialise a presence roster broadcast.
fn presence_json(editors: &[String]) -> String {
    serde_json::json!({ "op": "presence", "editors": editors }).to_string()
}

#[derive(serde::Deserialize)]
struct CodeGraphQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    map: Option<u8>,
}

#[derive(serde::Deserialize)]
struct RefsQuery {
    name: String,
}

#[derive(serde::Deserialize)]
struct GoalUpdateReq {
    goal: String,
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

#[derive(serde::Deserialize)]
struct PrActionReq {
    #[serde(default)]
    comment: String,
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

/// Max upload size (bytes) — generous for images/docs, bounded to protect disk.
const UPLOAD_MAX: usize = 25 * 1024 * 1024;

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

/// True for extensions whose MIME type a browser will execute as script if
/// the file is opened via direct/top-level navigation (SVG documents, HTML,
/// XML). `mime_of` derives Content-Type from the filename alone, so this
/// covers files stored through ANY upload path (avatar, chat attachment,
/// project attachment) — not just the one that first surfaced the bug.
fn is_active_content_ext(name: &str) -> bool {
    matches!(
        name.rsplit('.').next().map(str::to_lowercase).as_deref(),
        Some("svg" | "html" | "htm" | "xhtml" | "xml")
    )
}

/// Force a download instead of inline rendering for [`is_active_content_ext`]
/// files, so "open in new tab" / direct navigation can't execute embedded
/// script — the browser downloads the file rather than parsing it as a
/// top-level document.
fn force_download_if_active_content(file: &str, resp: &mut axum::response::Response) {
    if is_active_content_ext(file) {
        resp.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            axum::http::HeaderValue::from_static("attachment"),
        );
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

#[derive(serde::Deserialize)]
struct DmReq {
    user: String,
}

#[derive(serde::Deserialize)]
struct ReactReq {
    id: String,
    emoji: String,
}

#[derive(serde::Deserialize)]
struct WebhookReq {
    channel: String,
    #[serde(default)]
    label: String,
}

// ── Topic ──────────────────────────────────────────────────────────────────
#[derive(serde::Deserialize)]
struct TopicReq {
    topic: String,
}

#[derive(serde::Deserialize)]
struct HookPostReq {
    #[serde(default)]
    text: String,
    #[serde(default)]
    username: String,
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

#[derive(serde::Deserialize)]
struct ChatReplyReq {
    message: String,
}

#[derive(serde::Deserialize)]
struct PathQuery {
    #[serde(default)]
    path: String,
}

/// Resolve a user-supplied relative path under `root`, rejecting traversal
/// outside it. Returns the canonicalized path when safe.
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

#[derive(serde::Deserialize)]
struct InviteCreateReq {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    uses: Option<u32>,
}

#[derive(serde::Deserialize)]
struct JoinReq {
    token: String,
    username: String,
    password: String,
    #[serde(default)]
    name: String,
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

#[derive(serde::Deserialize)]
struct SprintGoalReq {
    goal: String,
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

#[derive(serde::Deserialize)]
struct CodeReq {
    code: String,
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
mod avatar_media_security_tests;
#[cfg(test)]
mod pr_preview_tests;
