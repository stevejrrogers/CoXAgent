// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Read models and config: state, metrics, workers, project settings.

use super::*;

pub(super) async fn build_state(
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

/// Serialize state for list views but drop each ticket's heavy `design` specs —
/// the board/backlog/roadmap only need the summary fields. Full specs load
/// on demand via [`ticket_detail_ep`], keeping the 1 Hz SSE payload small.
pub(super) fn lite_state_value(state: &coxagent_application::ProjectState) -> serde_json::Value {
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

pub(super) async fn state_ep(
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

pub(super) async fn metrics_ep(
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

/// The shared worker registry: every team (`account@host`) currently online for
/// this project, across all machines. Powers the dashboard's cross-machine view.
pub(super) async fn workers_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let workers = p.store.workers().await.unwrap_or_default();
    Json(workers).into_response()
}

pub(super) async fn get_config(
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

pub(super) async fn put_config(
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

/// Save an edited prompt (from the dashboard Settings prompt editor) to the
/// project-local `prompts/<role>.md`, so per-project overrides take effect
/// without touching the embedded defaults (CXA-F001). The role name is
/// validated against the manifest to avoid writing arbitrary paths.
pub(super) async fn save_prompt(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<PromptSaveReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let role = req.role.trim();
    if role.is_empty() || !is_valid_prompt_role(role) {
        return (axum::http::StatusCode::BAD_REQUEST, "unknown prompt role").into_response();
    }
    let path = p.work_dir.join("prompts").join(format!("{role}.md"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, req.content) {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "path": format!("prompts/{role}.md"),
        }))
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Request body for [`save_prompt`].
#[derive(serde::Deserialize)]
pub(super) struct PromptSaveReq {
    role: String,
    content: String,
}

/// Current content of a role prompt for the Settings editor: the project-local
/// `prompts/<role>.md` when present, else the embedded default — the same
/// resolution the engine uses, so the editor always shows what the agent would
/// actually receive (CXA-F001).
pub(super) async fn get_prompt(
    State(app): State<AppState>,
    Path((pid, role)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    if !is_valid_prompt_role(&role) {
        return (axum::http::StatusCode::BAD_REQUEST, "unknown prompt role").into_response();
    }
    let path = p.work_dir.join("prompts").join(format!("{role}.md"));
    let content = std::fs::read_to_string(&path)
        .ok()
        .or_else(|| coxagent_application::prompts::default_prompt_file(&format!("{role}.md")))
        .unwrap_or_default();
    Json(serde_json::json!({ "role": role, "content": content })).into_response()
}

fn is_valid_prompt_role(role: &str) -> bool {
    matches!(
        role,
        "ba" | "po" | "sm" | "sa" | "pd" | "dev" | "test" | "docs"
    )
}

pub(super) async fn set_priority(
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

/// WebRTC ICE servers for calls: a public STUN server, plus a TURN relay with
/// short-lived HMAC credentials when `COXAGENT_TURN_URL`/`_SECRET` are set
/// (coturn's `use-auth-secret` REST scheme). TURN lets calls traverse NATs that
/// block direct peer connections.
pub(super) async fn ice_config_ep() -> axum::response::Response {
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
