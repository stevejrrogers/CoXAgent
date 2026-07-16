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
use coxagent_application::DocPage;
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
    /// The code host for review actions (list/merge/diff PRs); set when git
    /// integration is configured with a provider.
    pub forge: Option<Arc<dyn coxagent_application::ports::outbound::ForgePort>>,
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

/// Hub-level, system-wide chat: one store shared across every project. Holds the
/// [`SystemChat`] aggregate (private channels + all messages) behind a mutex,
/// the JSON file it persists to, a broadcast bus for live WebSockets, and the
/// directory uploaded chat media lives in.
#[derive(Clone)]
struct SysChat {
    inner: Arc<tokio::sync::Mutex<coxagent_application::SystemChat>>,
    path: PathBuf,
    tx: tokio::sync::broadcast::Sender<String>,
}

impl SysChat {
    /// Load the store from `dir/system_chat.json` (empty if absent).
    fn load(dir: &std::path::Path) -> Self {
        let path = dir.join("system_chat.json");
        let inner = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            tx: tokio::sync::broadcast::channel(256).0,
        }
    }

    /// Persist the current state to disk (best-effort).
    async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, json);
    }
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
}

/// Assemble the shared [`AppState`] from the registered projects and hub extras.
fn build_state(
    projects: Vec<ProjectHandle>,
    audit: Arc<dyn AuditPort>,
    extras: HubExtras,
) -> AppState {
    let order: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();
    let map: HashMap<String, ProjectHandle> =
        projects.into_iter().map(|p| (p.id.clone(), p)).collect();
    let hub_dir = extras.hub_dir.unwrap_or_else(|| PathBuf::from("."));
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
        syschat: SysChat::load(&hub_dir),
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
    let state = build_state(projects, audit, extras);

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
        .route("/api/chat/messages", get(syschat_messages_ep))
        .route("/api/chat/members", get(syschat_members_ep))
        .route("/api/chat/dm", post(syschat_dm_ep))
        .route("/api/chat/react", post(syschat_react_ep))
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
        .route("/api/tooling", get(tooling_ep))
        .route("/api/analyze-goal", post(analyze_goal_ep))
        .route("/api/projects", get(list_projects).post(create_project))
        .route(
            "/api/projects/:pid",
            axum::routing::delete(delete_project_ep).patch(rename_project_ep),
        )
        .route("/api/projects/:pid/state", get(state_ep))
        .route("/api/projects/:pid/metrics", get(metrics_ep))
        .route("/api/projects/:pid/runner", get(runner_ep))
        .route("/api/projects/:pid/workers", get(workers_ep))
        .route("/api/token-saver", get(token_saver_ep))
        .route("/api/projects/:pid/audit", get(audit_ep))
        .route("/api/projects/:pid/config", get(get_config).put(put_config))
        .route("/api/projects/:pid/control/:action", post(control_ep))
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
        .route("/api/projects/:pid/codegraph", get(codegraph_ep))
        .route("/api/projects/:pid/codegraph/refs", get(codegraph_refs_ep))
        .route("/api/projects/:pid/codegraph/deps", get(codegraph_deps_ep))
        .route(
            "/api/projects/:pid/codegraph/build",
            post(codegraph_build_ep),
        )
        .route("/api/projects/:pid/standup", post(standup_ep))
        .route("/api/projects/:pid/tickets", post(create_ticket))
        .route("/api/projects/:pid/ticket/:id", get(ticket_detail_ep))
        .route("/api/projects/:pid/ticket/:id/priority", post(set_priority))
        .route("/api/projects/:pid/ticket/:id/reject", post(reject_ticket))
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
        .route("/api/projects/:pid/prs", get(list_prs_ep))
        .route("/api/projects/:pid/prs/:num/diff", get(pr_diff_ep))
        .route("/api/projects/:pid/prs/:num/:action", post(pr_action_ep))
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
    // (Director/Manager/*Lead). Everyone else is forbidden.
    if let Some(auth) = &app.auth {
        let allowed = match resolve_principal(auth, &headers).await {
            Some(u) => u.role == coxagent_application::auth::AuthRole::Admin || u.role.is_lead(),
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
                        samples += 1;
                        before += b;
                        after += a;
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
    Json(req): Json<PostCommentReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let body = req.body.trim();
    if body.is_empty() && req.attachments.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "empty comment").into_response();
    }
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    state.post_comment_att("USER", body, req.ticket.clone(), req.attachments.clone());
    if let Err(e) = p.store.save(&state).await {
        return internal_error(&e.to_string());
    }
    // If the user attached something an agent can read, let the SA agent read it
    // and respond — answering if a question was asked, otherwise reading it
    // proactively and asking back. Runs in the background so the post is instant.
    maybe_analyze_attachments(&app, &p, "USER", body, req.ticket, &req.attachments);
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
        sc.create_channel(&req.name, &user, &ctx)
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
    match action.as_str() {
        "resume" => {
            // Attribute the run to whoever started it, on this machine, so the
            // agent cards can show "account@host" and claims are owned correctly.
            // A headless worker (no login session) takes its identity from
            // COXAGENT_OPERATOR — the way a `coxagent run` box on another machine
            // gets a distinct name in the shared registry.
            let account = match std::env::var("COXAGENT_OPERATOR") {
                Ok(o) if !o.is_empty() => o,
                _ => resolve_username(&app, &headers).await,
            };
            p.runner.set_operator(&account, &machine_host());
            p.runner.resume();
        }
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
async fn auth_mw(
    State(app): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return next.run(req).await;
    };
    let path = req.uri().path().to_owned();
    // Public routes: the SPA shell, health, login, and incoming webhooks (the
    // webhook token is the credential, so no session is required).
    if path == "/"
        || path == "/api/health"
        || path == "/api/auth/login"
        || path.starts_with("/api/chat/hook/")
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
        && !path.ends_with("/chat") // team chat is open to any signed-in user
        && path != "/api/chat/send" // system chat send: any signed-in user
        && path != "/api/chat/dm" // open a DM: any signed-in user
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
    Json(req): Json<CreateUserReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
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
    if req.password.len() < 4 {
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
    // The People view lists a project's members to anyone on the project.
    let allowed = resolve_principal(&auth, &headers)
        .await
        .is_some_and(|u| u.role.can_write());
    if !allowed {
        return (StatusCode::FORBIDDEN, "sign-in required").into_response();
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
            Json(serde_json::json!({
                "auth": true, "username": u.username,
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
