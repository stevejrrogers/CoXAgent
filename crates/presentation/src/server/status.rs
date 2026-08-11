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

/// The Settings screen's view of `coxagent.json`.
///
/// A field the file cannot supply is defaulted ON ITS OWN (COX-B050). Handing
/// back a whole default config because of one bad field is worse here than
/// anywhere else: this screen PUTs back what it was shown, so a single
/// out-of-range port would turn into every other setting being overwritten
/// with defaults on disk the next time anyone pressed Save.
pub(super) async fn get_config(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let text = std::fs::read_to_string(&p.config_path).ok();
    Json(config_for_display(text.as_deref())).into_response()
}

/// What the Settings screen should show for a config file that reads as
/// `text` (`None` = no file). Pure, so the rule that one bad field costs one
/// field is testable without a project fixture.
fn config_for_display(text: Option<&str>) -> Config {
    text.and_then(|t| coxagent_application::salvage_config(t).ok())
        .map_or_else(Config::default, |salvaged| salvaged.config)
}

/// The Settings screen's warning banner, as a pure function of the file text:
/// the fields that had to be defaulted, plus why the file could not be read
/// at all when it could not.
fn config_health(text: Option<&str>) -> (Vec<coxagent_application::ConfigDefect>, Option<String>) {
    // No file is not a fault: a project that never wrote one runs on defaults
    // by design.
    match text {
        None => (Vec::new(), None),
        Some(text) => match coxagent_application::salvage_config(text) {
            Ok(salvaged) => (salvaged.defects, None),
            Err(e) => (Vec::new(), Some(e)),
        },
    }
}

/// What [`get_config`] could NOT read out of `coxagent.json`, so the Settings
/// screen can say so instead of presenting silently-defaulted values as if the
/// operator had chosen them. `unreadable` is set when the file is not JSON at
/// all, where there is nothing to salvage field by field.
pub(super) async fn config_defects_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let text = std::fs::read_to_string(&p.config_path).ok();
    let (defects, unreadable) = config_health(text.as_deref());
    Json(serde_json::json!({ "defects": defects, "unreadable": unreadable })).into_response()
}

pub(super) async fn put_config(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(cfg): Json<Config>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Budget caps apply instantly (shared live cell). The engine/model config is
    // picked up by the runner at its next cycle boundary — it re-reads
    // coxagent.json and hot-reloads the engine, no restart. Only a few
    // process-captured knobs (the loop's own worker identity) still need one.
    if let Ok(mut caps) = p.budget.lock() {
        caps.lifetime_usd = cfg.workflow.budget_usd;
        caps.daily_usd = cfg.policy.daily_budget_usd;
    }
    match serde_json::to_string_pretty(&cfg) {
        Ok(text) => match std::fs::write(&p.config_path, text) {
            Ok(()) => Json(serde_json::json!({
                "ok": true,
                "note": "budget applied live; engine/model apply on the next cycle (no restart)"
            }))
            .into_response(),
            Err(e) => internal_error(&e.to_string()),
        },
        Err(e) => internal_error(&e.to_string()),
    }
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

#[cfg(test)]
mod config_display_tests {
    use super::{config_for_display, config_health};

    /// A config document with one unreadable field beside good ones.
    fn with_bad_port() -> String {
        let mut cfg =
            serde_json::to_value(super::Config::default()).expect("defaults serialize as json");
        cfg["deploy"] = serde_json::json!({
            "host_port": 99_999,
            "auto_rollback": true,
            "max_rollback_age_secs": 42,
        });
        serde_json::to_string(&cfg).expect("config text")
    }

    /// AC (COX-B050): the Settings screen must show what the project is
    /// actually running on. Showing defaults for the whole file because of one
    /// bad field is doubly wrong here — this screen PUTs back what it shows,
    /// so the next Save would write those defaults over the operator's file.
    #[test]
    fn one_bad_field_does_not_turn_settings_into_a_default_config() {
        let cfg = config_for_display(Some(&with_bad_port()));

        assert!(cfg.deploy.auto_rollback, "shown as configured");
        assert_eq!(cfg.deploy.max_rollback_age_secs, 42);
        assert_eq!(
            cfg.deploy.host_port, None,
            "only the bad field is defaulted"
        );
    }

    /// And the screen is told which field it is showing a default for, so the
    /// defaulting is never silent.
    #[test]
    fn the_defaulted_field_is_named_for_the_operator() {
        let (defects, unreadable) = config_health(Some(&with_bad_port()));

        assert_eq!(unreadable, None);
        assert_eq!(
            defects.iter().map(|d| d.path.as_str()).collect::<Vec<_>>(),
            vec!["deploy.host_port"]
        );
    }

    /// A file that is not JSON at all has no field to blame: say the file is
    /// unreadable rather than presenting defaults as the operator's settings.
    #[test]
    fn a_file_that_is_not_json_is_reported_as_unreadable() {
        let (defects, unreadable) = config_health(Some("{\"deploy\": "));

        assert!(defects.is_empty());
        assert!(unreadable.is_some());
    }

    /// A project with no config file, and one with a good one, are both quiet
    /// — the banner must not cry wolf on a healthy install.
    #[test]
    fn a_missing_or_healthy_config_reports_nothing() {
        assert_eq!(config_health(None), (Vec::new(), None));

        let good = serde_json::to_string(&super::Config::default()).expect("config text");
        assert_eq!(config_health(Some(&good)), (Vec::new(), None));
    }
}
