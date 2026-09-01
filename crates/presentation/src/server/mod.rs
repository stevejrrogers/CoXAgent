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
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::{Stream, StreamExt};

use crate::middleware::{
    cors_layer, rate_limit_mw, telemetry_mw, RateLimiter, AUTH_RATE_MAX, AUTH_RATE_WINDOW,
};

mod alerts;
mod approval_policy;
mod assets;
mod auth;
mod background;
mod broken_projects;
mod channels;
mod chat;
mod comments;
mod deps;
mod docs;
mod downloads;
mod engines;
mod factory;
mod fleet;
mod fleet_spend;
mod forge;
mod goals;
mod guards;
mod hub_docs;
mod inbox;
mod lessons;
mod manage;
mod meetings;
mod metrics_admin;
mod openapi;
mod people;
mod pr_listing;
mod preflight;
mod projects;
mod realtime;
mod repro_url;
mod requests;
mod search;
mod security;
mod share_link;
mod share_page;
mod status;
mod store_rpc;
mod transcripts;
mod tunecockpit;
mod work;

use alerts::*;
use approval_policy::*;
use assets::*;
use auth::*;
use background::*;
pub use broken_projects::BrokenProject;
use broken_projects::*;
use channels::*;
use chat::*;
use comments::*;
use docs::*;
use downloads::*;
use engines::*;
use fleet::*;
use fleet_spend::*;
use forge::*;
use guards::*;
use hub_docs::*;
use inbox::*;
use manage::*;
use meetings::*;
use metrics_admin::spawn_metrics_admin;
use openapi::*;
use people::*;
use pr_listing::*;
use preflight::*;
use projects::*;
use realtime::*;
use repro_url::*;
use requests::*;
use search::*;
use security::*;
use share_link::*;
use share_page::*;
use status::*;
use transcripts::*;
use tunecockpit::*;
use work::*;

/// The embedded single-page dashboard.
const INDEX_HTML: &str = include_str!("../web/index.html");
// Vendored terminal assets — embedded so the terminal works offline/air-gapped.
const XTERM_JS: &str = include_str!("../web/xterm.min.js");
const XTERM_CSS: &str = include_str!("../web/xterm.min.css");
const XTERM_FIT_JS: &str = include_str!("../web/xterm-addon-fit.min.js");
// The dashboard's own split assets — one CSS file plus classic scripts in
// load order (they share one global scope; the split is for merge-conflict
// surface, not modularity). Embedded like everything else: one binary.
const APP_CSS: &str = include_str!("../web/app.css");
// Vendored Tabler icons webfont (pinned v3.24.0, MIT) — embedded for the same
// reason as Mermaid above: deployed containers have no CDN egress, and a
// webfont that fails to load leaves every `.ti-*` glyph with zero ink (the
// icon characters exist only as CSS `content`, so there is no text fallback).
// CXA-B112: the sign-in CTA rendered text-only in deploys for exactly this.
const TABLER_CSS: &str = include_str!("../web/tabler-icons.min.css");
const TABLER_WOFF2: &[u8] = include_bytes!("../web/fonts/tabler-icons.woff2");
const APP_JS: &[(&str, &str)] = &[
    // Vendored Mermaid (pinned v11 UMD build) so Wiki pages render
    // sequence/flow diagrams offline — the hub never loads from a CDN.
    ("mermaid.min.js", include_str!("../web/js/mermaid.min.js")),
    ("core.js", include_str!("../web/js/core.js")),
    ("manage.js", include_str!("../web/js/manage.js")),
    ("home.js", include_str!("../web/js/home.js")),
    ("river.js", include_str!("../web/js/river.js")),
    ("chat.js", include_str!("../web/js/chat.js")),
    // Global search palette (CXA-F275) — extracted from chat.js so the box
    // used from every view has one home. Load after chat.js (runtime refs).
    ("search.js", include_str!("../web/js/search.js")),
    ("mcp.js", include_str!("../web/js/mcp.js")),
    ("docs.js", include_str!("../web/js/docs.js")),
    ("inbox.js", include_str!("../web/js/inbox.js")),
    ("drift.js", include_str!("../web/js/drift.js")),
    ("alerts.js", include_str!("../web/js/alerts.js")),
    // Approval-policy transparency panel (CXA-F303) — extends the Settings
    // Workflow tab; loads before shell.js like every view helper.
    (
        "approval_policy.js",
        include_str!("../web/js/approval_policy.js"),
    ),
    // Lesson efficacy panel (CXA-F306) — the Overview recurrence view; loads
    // before shell.js like every view helper.
    ("lessons.js", include_str!("../web/js/lessons.js")),
    ("shell.js", include_str!("../web/js/shell.js")),
];

/// One of the split dashboard scripts, by basename.
async fn app_js_ep(Path(name): Path<String>) -> axum::response::Response {
    match APP_JS.iter().find(|(n, _)| *n == name) {
        Some((_, body)) => (
            [("content-type", "application/javascript; charset=utf-8")],
            *body,
        )
            .into_response(),
        None => not_found(),
    }
}

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
    /// Blob storage (MinIO/S3 or the local blob dir) for ticket attachments
    /// and evidence media.
    pub storage: Option<Arc<dyn coxagent_application::ports::outbound::StoragePort>>,
    /// Durable outbound-alert spool (CXA-F235): powers the operator's
    /// delivery-history view and one-click replay. `None` where the
    /// composition root has no webhook sink to spool for.
    pub outbox: Option<Arc<dyn coxagent_application::ports::outbound::OutboxStorePort>>,
    /// Workspace file access for on-demand reviews; injected by the
    /// composition root so this layer stays free of infrastructure.
    pub files: Option<Arc<dyn coxagent_application::ports::outbound::WorkspaceFilesPort>>,
    /// Lockfile discovery for the dependency-health scan (CXA-B111); injected
    /// by the composition root so this layer stays free of infrastructure.
    pub deps_discovery:
        Option<Arc<dyn coxagent_application::ports::outbound::DependencyDiscoveryPort>>,
}

/// Builds a fresh project on demand (scaffold + register), injected by the
/// composition root so the presentation layer stays free of infrastructure.
/// The contract lives in [`factory`]; re-exported here because every
/// submodule globs `super::*` and `lib.rs` re-exports the names.
pub use factory::{FactoryError, FactoryErrorKind, NewProjectReq, ProjectFactory, ProjectRemover};

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

#[allow(clippy::cast_possible_truncation)] // the low 32 bits of the hash IS the value
fn rand_u32() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish() as u32
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
    /// Registered projects that could not be loaded, kept so the listing can
    /// name them and their reason (COX-B043). Live, not frozen at boot: the
    /// composition root retries failed loads (CXA-B114) and admits a
    /// recovered project, whose entry is cleared here the moment it lands.
    broken: Arc<RwLock<Vec<BrokenProject>>>,
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
                    // Super counts as admin here: comparing the string to
                    // "admin" alone hid every PROJECT channel from the hub
                    // owner, who is the one person guaranteed to want them.
                    admin: u.role.can_manage(),
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
    /// Registered projects that failed to load (e.g. an unparseable
    /// `coxagent.json`), so the dashboard can show why one is missing instead
    /// of silently omitting it — COX-B043.
    pub broken: Vec<BrokenProject>,
    /// Inbox for projects the composition root recovered after boot
    /// (CXA-B114): a failed store connect is retried in the background, and
    /// when it succeeds the live handle arrives here to join the registry
    /// without a restart.
    pub recoveries: Option<tokio::sync::mpsc::Receiver<ProjectHandle>>,
}

/// Warn threshold for a space's budget, matching the dashboard's own amber one
/// (index.html renders the "nearly reached" alert at 80% of a project's cap) —
/// same UX language, just at the space level and pushed as a chat heads-up.
const WARN_PCT: f64 = 0.8;

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
    mut extras: HubExtras,
) -> std::io::Result<()> {
    let backup_dir = extras
        .hub_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("backups");
    // Taken out before build_state (which consumes the rest of `extras`): a
    // mpsc Receiver cannot live inside the Clone-able AppState — it is
    // drained by exactly one background task instead.
    let recoveries = extras.recoveries.take();
    let state = build_state(projects, audit, extras).await;
    // CXA-B114: a project that failed to load at boot (e.g. the DB was still
    // starting) is rebuilt by the composition root; when it recovers, the
    // handle arrives here and joins the live registry — no restart.
    if let Some(recoveries) = recoveries {
        tokio::spawn(admit_recovered_projects(state.clone(), recoveries));
    }
    tracing::info!("hub role: {:?}", hub_role());
    // Batch/watchdog loops belong to the knowledge role (and the all-in-one).
    if matches!(hub_role(), HubRole::All | HubRole::Knowledge) {
        // Space budget enforcement runs for the life of the hub.
        tokio::spawn(space_budget_watchdog(state.clone()));
        // Hub-level daily soft-ceiling alert (CXA-F278): notify-only, ever.
        tokio::spawn(fleet_ceiling_watchdog(state.clone()));
        // Meeting reminders, start announcements, and absent-participant rings.
        tokio::spawn(meeting_watchdog(state.clone()));
        // Nightly snapshots of the hub-level documents (workspace, spaces, chat).
        tokio::spawn(nightly_backup(state.clone(), backup_dir));
        // App-release watcher: new tagged builds surface as update notices.
        tokio::spawn(releases_watchdog(state.clone()));
        // Loop-liveness watchdog (CXA-F259): alerts when a running loop goes
        // silently stale — the blind spot a hung cycle leaves (the worker
        // keeps beating its registry heartbeat while nothing progresses), and
        // the checker runs HERE, outside the unit that can hang.
        tokio::spawn(liveness_watchdog(state.clone()));
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
            "/assets/app.css",
            get(|| async { ([("content-type", "text/css; charset=utf-8")], APP_CSS) }),
        )
        .route("/assets/js/:name", get(app_js_ep))
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
        .route(
            "/assets/tabler-icons.min.css",
            get(|| async { ([("content-type", "text/css; charset=utf-8")], TABLER_CSS) }),
        )
        .route(
            "/assets/fonts/tabler-icons.woff2",
            get(|| async { ([("content-type", "font/woff2")], TABLER_WOFF2) }),
        )
        .route("/api/health", get(health))
        .route("/api/openapi.json", get(openapi_ep))
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
        .route(
            "/api/chat/channels/:cid",
            axum::routing::delete(syschat_delete_channel_ep),
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
        // Cross-project live agent activity river (CXA-F233): every registered
        // project's runner phase + activity in ONE SSE stream, filterable by
        // project id and agent phase (see fleet.rs).
        .route("/api/fleet/river", get(fleet_river_ep))
        // Fleet spend cockpit (CXA-F278): hub-level cross-project cost
        // aggregation with cap headroom + the soft-ceiling setting (see
        // fleet_spend.rs). Super admin; visibility-only by design.
        .route("/api/fleet/spend", get(fleet_spend_ep))
        .route(
            "/api/fleet/ceiling",
            axum::routing::put(fleet_ceiling_put_ep),
        )
        .route("/api/tooling", get(tooling_ep))
        // Global search (CXA-F275): one box across tickets, wiki pages and
        // chat threads. No :pid in the path — the handler enforces project
        // scope itself (see server/search.rs).
        .route("/api/search", get(global_search_ep))
        .route("/api/analyze-goal", post(analyze_goal_ep))
        .route("/api/projects", get(list_projects).post(create_project))
        .route(
            "/api/projects/:pid",
            axum::routing::delete(delete_project_ep).patch(rename_project_ep),
        )
        .route(
            "/api/projects/:pid/store",
            post(store_rpc::store_rpc_ep).get(store_rpc::store_audit_ep),
        )
        .route("/api/projects/:pid/state", get(state_ep))
        .route("/api/projects/:pid/preflight", get(preflight_ep))
        .route("/api/projects/:pid/dependencies", get(dependencies_ep))
        .route("/api/projects/:pid/metrics", get(metrics_ep))
        .route(
            "/api/projects/:pid/milestones/projection",
            get(milestones_projection_ep),
        )
        .route(
            "/api/projects/:pid/milestone-complete/:name",
            post(milestone_complete_ep),
        )
        .route(
            "/api/projects/:pid/metrics/summary",
            get(metrics_summary_ep),
        )
        .route("/api/projects/:pid/metrics/trends", get(metrics_trends_ep))
        .route(
            "/api/projects/:pid/metrics/burndown",
            get(metrics_burndown_ep),
        )
        .route("/api/projects/:pid/agent-evals", get(agent_evals_ep))
        .route("/api/projects/:pid/runner", get(runner_ep))
        .route("/api/projects/:pid/workers", get(workers_ep))
        .route("/api/token-saver", get(token_saver_ep))
        .route("/api/projects/:pid/audit", get(audit_ep))
        .route("/api/projects/:pid/alerts", get(list_alerts_ep))
        .route(
            "/api/projects/:pid/alerts/:id/replay",
            post(replay_alert_ep),
        )
        .route("/api/projects/:pid/config", get(get_config).put(put_config))
        .route("/api/projects/:pid/control/:action", post(control_ep))
        .route("/api/projects/:pid/burn-mode", post(burn_mode_ep))
        .route("/api/projects/:pid/brakes", get(brakes_ep))
        .route(
            "/api/projects/:pid/brakes/:brake/hold",
            post(brake_hold_ep).delete(brake_hold_clear_ep),
        )
        .route(
            "/api/projects/:pid/approval-policy",
            get(approval_policy_ep),
        )
        .route(
            "/api/projects/:pid/approval-policy/ask-again",
            post(approval_policy_ask_again_ep),
        )
        .route(
            "/api/projects/:pid/approval-policy/release",
            post(approval_policy_release_ep),
        )
        .route(
            "/api/projects/:pid/lessons/dismiss",
            post(lessons::lessons_dismiss_ep),
        )
        .route(
            "/api/projects/:pid/lessons/escalate",
            post(lessons::lessons_escalate_ep),
        )
        .route("/api/projects/:pid/sprint/goal", post(set_sprint_goal_ep))
        .route("/api/projects/:pid/sprint/close", post(sprint_close_ep))
        .route("/api/projects/:pid/sprint-queue", post(queue_sprint_ep))
        .route(
            "/api/projects/:pid/sprint-queue/:qid/scope",
            post(queue_scope_ep),
        )
        .route(
            "/api/projects/:pid/sprint-queue/:qid/rename",
            post(queue_rename_ep),
        )
        .route(
            "/api/projects/:pid/sprint-queue/:qid/move/:dir",
            post(queue_move_ep),
        )
        .route(
            "/api/projects/:pid/sprint-queue/:qid",
            axum::routing::delete(queue_delete_ep),
        )
        .route("/api/projects/:pid/sprint/:action", post(sprint_scope_ep))
        .route("/api/projects/:pid/digest", post(digest_ep))
        .route("/api/projects/:pid/deps/scan", post(deps::scan_ep))
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
        // Public share-link status page (CXA-F069): the token IS the
        // credential, so no session is required (allowlisted in auth_mw).
        .route("/s/:token", get(share_page_ep))
        // Share-link management (admin): mint, list, revoke.
        .route(
            "/api/projects/:pid/share-links",
            get(share_link_list_ep).post(share_link_create_ep),
        )
        .route(
            "/api/projects/:pid/share-links/:token",
            axum::routing::delete(share_link_revoke_ep),
        )
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
            "/api/projects/:pid/ticket/:id/status/:action",
            post(hold_ticket_ep),
        )
        .route(
            "/api/projects/:pid/ticket/:id/approve-cost",
            post(approve_cost),
        )
        .route("/api/projects/:pid/inbox", get(inbox_ep))
        .route("/api/projects/:pid/goals", post(goals::add_goal_ep))
        .route("/api/projects/:pid/goals/outcomes", get(goals::outcomes_ep))
        .route(
            "/api/projects/:pid/goals/:gid/rename",
            post(goals::rename_goal_ep),
        )
        .route(
            "/api/projects/:pid/ticket/:id/goal",
            post(goals::ticket_set_goal_ep),
        )
        .route("/api/projects/:pid/pr/:number/human", post(human_pr_ep))
        .route("/api/projects/:pid/reverts/:sha", post(revert_decision_ep))
        .route("/api/projects/:pid/attachment", get(attachment_ep))
        .route(
            "/api/projects/:pid/ticket/:id/attachments",
            post(upload_attachment_ep)
                .delete(delete_attachment_ep)
                .layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024)),
        )
        .route("/api/projects/:pid/ticket/:id/ready", post(human_ready_ep))
        .route(
            "/api/projects/:pid/ticket/:id/verify",
            post(human_verify_ep),
        )
        .route(
            "/api/projects/:pid/ticket/:id/reproduction-url",
            get(ticket_reproduction_url_ep),
        )
        .route(
            "/api/projects/:pid/ticket/:id/send-back",
            post(send_back_ep),
        )
        .route(
            "/api/projects/:pid/ticket/:id/assign",
            post(assign_ticket_ep),
        )
        .route(
            "/api/projects/:pid/ticket/:id/undo-approval",
            post(undo_approval_ep),
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
        .route("/api/pr-report", post(pr_report_ep))
        .route("/api/pr-report/reviews", get(pr_reviews_ep))
        .route("/api/projects/:pid/prs/:num/diff", get(pr_diff_ep))
        .route("/api/projects/:pid/prs/:num/:action", post(pr_action_ep))
        .route("/api/projects/:pid/agent-log", get(agent_log_ep))
        .route(
            "/api/projects/:pid/agent-log/stream",
            get(agent_log_stream_ep),
        )
        .route("/api/projects/:pid/transcripts", get(list_transcripts))
        .route("/api/projects/:pid/transcripts/:name", get(get_transcript))
        .route("/api/projects/:pid/events", get(events_ep))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))
        .layer(axum::middleware::from_fn(security_headers_mw))
        .with_state(state);

    // --- CORS layer ---
    // Applied only when COXAGENT_CORS_ORIGINS is set; unset/empty = no CORS
    // headers (same-origin + SameSite=Strict cookies guard the app as before).
    let cors_origins = std::env::var("COXAGENT_CORS_ORIGINS").unwrap_or_default();
    let app = if let Some(cors) = cors_layer(&cors_origins) {
        app.layer(cors)
    } else {
        app
    };

    // --- Auth rate-limit layer ---
    // Applies a per-IP sliding-window limit to all /api/auth/ routes.
    // COXAGENT_TRUST_PROXY=1 reads the client IP from X-Forwarded-For (LB
    // topology); default is TCP peer address (safe for direct exposure).
    let trust_proxy = std::env::var("COXAGENT_TRUST_PROXY").ok().as_deref() == Some("1");
    let limiter = Arc::new(RateLimiter::new());
    let app = app.layer(axum::middleware::from_fn(move |req, next| {
        rate_limit_mw(
            req,
            next,
            Arc::clone(&limiter),
            AUTH_RATE_MAX,
            AUTH_RATE_WINDOW,
            trust_proxy,
        )
    }));

    // --- HTTP telemetry layer (outermost, CXA-C039) ---
    // Sits outside CORS/rate-limit/auth so the recorded status is the one the
    // client actually sees (429s included), exactly once per request. The
    // registry is shared with the metrics admin listener below; its creation
    // also starts the uptime clock.
    let registry = Arc::new(coxagent_application::MetricsRegistry::new());
    let telemetry_registry = Arc::clone(&registry);
    let app = app.layer(axum::middleware::from_fn(move |req, next| {
        telemetry_mw(req, next, Arc::clone(&telemetry_registry))
    }));

    // --- Metrics admin listener (CXA-C039) ---
    // Served by gateway/realtime roles (and the all-in-one); knowledge pods
    // run batch loops only — same surface rule as the Redis bus bridge.
    if !matches!(hub_role(), HubRole::Knowledge) {
        spawn_metrics_admin(registry);
    }

    // Bind loopback by default (safe for local use); a container sets
    // COXAGENT_HOST=0.0.0.0 so published ports are reachable from the host.
    let host = std::env::var("COXAGENT_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("dashboard on http://{addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

async fn index() -> impl IntoResponse {
    // Cache-bust the split assets per build: the desktop WebView happily kept
    // an older shell.js against a newer index.html across redeploys, which
    // broke whole views (Code map went blank). The version query makes every
    // build a fresh URL.
    static VERSIONED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        // Keyed on the asset CONTENT, not the crate version: two builds of
        // the same version ship different CSS, and a stale WebView cache
        // painted the DM list on top of the channel tree for exactly that
        // reason. Hash changes ⇒ URL changes ⇒ refetch.
        let v = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            APP_CSS.hash(&mut h);
            TABLER_CSS.hash(&mut h);
            for (_, body) in APP_JS {
                body.hash(&mut h);
            }
            INDEX_HTML.hash(&mut h);
            format!("{:x}", h.finish())
        };
        INDEX_HTML
            .replace("/assets/app.css", &format!("/assets/app.css?v={v}"))
            .replace(
                "/assets/tabler-icons.min.css",
                &format!("/assets/tabler-icons.min.css?v={v}"),
            )
            .replace(".js\"></script>", &format!(".js?v={v}\"></script>"))
    });
    // Always revalidate so a rebuilt dashboard is picked up on reload (the SPA is
    // small; no-cache avoids stale UI after an upgrade).
    //
    // CSP + hardening headers. The dashboard uses inline <script>/<style> (a
    // single embedded file) so 'unsafe-inline' is required there; the Inter
    // font still comes from Google Fonts, so that host is allow-listed for
    // style/font. The Tabler icon webfont is vendored (served from 'self'),
    // like Mermaid and xterm, so deploys without CDN egress still get icons.
    // Everything else is locked to same-origin, WebSocket to self,
    // images/fonts to data:, and framing is denied.
    const CSP: &str = "default-src 'self'; \
        script-src 'self' 'unsafe-inline'; \
        style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
        font-src 'self' data: https://fonts.gstatic.com; \
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
        Html(VERSIONED.as_str()),
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

/// Max characters accepted in a single chat message.
const CHAT_MAX_CHARS: usize = 2000;
/// Sliding-window rate limit for a single WebSocket: at most this many messages
/// per [`CHAT_RATE_WINDOW`].
const CHAT_RATE_MAX: usize = 12;
const CHAT_RATE_WINDOW: Duration = Duration::from_secs(10);

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

/// Turn the human burn mode (CXA-F030) on/off and set its numeric exit gate.
#[derive(serde::Deserialize)]
struct BurnModeReq {
    enabled: bool,
    /// Clear the mode by itself once the open-bug count reaches this;
    /// `null` keeps it on until switched off by hand.
    #[serde(default)]
    target: Option<u32>,
}

/// Which tickets to pull into (or drop from) the running sprint.
#[derive(serde::Deserialize)]
struct SprintScopeReq {
    tickets: Vec<String>,
}

/// Why a ticket is being put on hold.
#[derive(serde::Deserialize)]
struct HoldReq {
    #[serde(default)]
    reason: String,
}

/// A sprint queued to run after the current one (goal + optional ticket picks).
#[derive(serde::Deserialize)]
struct QueueSprintReq {
    goal: String,
    #[serde(default)]
    tickets: Vec<String>,
}

/// Ticket adds/removes on one queued sprint.
#[derive(serde::Deserialize)]
struct QueueScopeReq {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
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

/// An expected client conflict (CXA-B129): the same JSON error shape as
/// [`internal_error`], but 409 so a client can react instead of retry-blind
/// against what looks like a server fault.
fn conflict_error(msg: &str) -> axum::response::Response {
    (
        axum::http::StatusCode::CONFLICT,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

/// Invalid client input (CXA-B138, CXA-B139): the same JSON error shape, but
/// 400 so the client learns the request itself was bad — retrying can never
/// succeed.
fn bad_request_error(msg: &str) -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

#[cfg(test)]
mod alerts_tests;
#[cfg(test)]
mod approval_policy_tests;
#[cfg(test)]
mod avatar_media_security_tests;
#[cfg(test)]
mod cors_rate_limit_tests;
#[cfg(test)]
mod delete_project_tests;
#[cfg(test)]
mod pr_preview_tests;
#[cfg(test)]
mod pr_review_gate_tests;
#[cfg(test)]
mod project_create_tests;
#[cfg(test)]
mod repro_url_tests;
#[cfg(test)]
mod share_link_tests;
#[cfg(test)]
mod store_rpc_audit_tests;
#[cfg(test)]
mod store_rpc_auth_enforcement_tests;
#[cfg(test)]
mod store_rpc_guard_tests;
#[cfg(test)]
mod store_rpc_stale_write_tests;
#[cfg(test)]
mod store_rpc_test_support;
#[cfg(test)]
mod ui_contrast_tests;
