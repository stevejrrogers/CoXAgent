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
        broken: Arc::new(extras.broken),
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
        // All four BUILT-IN channels are public by construction (`#general`
        // for people, `#agents`/`#approvals`/`#incidents` for the machine's
        // announcements). Filtering the snapshot down to `#general` alone made
        // the SM's coordination invisible — the dashboard looked like a team
        // that never talks. Only user-created channels (which carry member
        // lists) stay off the broadcast.
        const PUBLIC: [&str; 4] = [
            coxagent_application::GENERAL_CHANNEL,
            coxagent_application::state::AGENTS_CHANNEL,
            coxagent_application::state::APPROVALS_CHANNEL,
            coxagent_application::state::INCIDENTS_CHANNEL,
        ];
        chat.retain(|m| {
            m.get("channel")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|c| PUBLIC.contains(&c))
        });
        // The snapshot broadcasts EVERY second: ship only each channel's tail
        // (newest 50) — history beyond that comes from the paginated REST
        // list. Bounded-500 chat serialized 4 channels per tick was a real
        // drag on the chat pane.
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let keep: Vec<bool> = chat
            .iter()
            .rev()
            .map(|m| {
                let c = m
                    .get("channel")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let n = seen.entry(c).or_default();
                *n += 1;
                *n <= 50
            })
            .collect();
        let mut keep_fwd = keep;
        keep_fwd.reverse();
        let mut it = keep_fwd.into_iter();
        chat.retain(|_| it.next().unwrap_or(false));
    }
    // Dependency radar (CXA-F237): read-only derived summary on top of the
    // SAME snapshot served today — why each Ready ticket is not running (the
    // full blocking chain, powering the backlog BLOCKED badge) and the
    // critical path to the next release. Pure derivation, no persisted
    // change. A project with nothing to report emits no `derived` key at all,
    // so clients treat absence as an empty radar rather than an error.
    let radar = coxagent_application::dependency_radar::radar(state);
    let derived = serde_json::to_value(&radar).unwrap_or_default();
    if derived.as_object().is_some_and(|o| !o.is_empty()) {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("derived".into(), derived);
        }
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

/// The project's dependency graph (CXA-F237 AC5), derived ONLY from
/// `ProjectState`: nodes are exactly the tickets in state with their live
/// status, edges exactly the declared `depends_on` pairs — nothing fabricated
/// (an edge to an id the state does not know is still served; the absent id
/// simply has no node and surfaces as unknown). `cycle` flags members of a
/// `depends_on` cycle so the render can mark them instead of hanging on them.
pub(super) async fn dependencies_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match p.store.load().await {
        Ok(state) => {
            use coxagent_application::dependency_radar::{cycle_members, dependency_graph};
            let (nodes, edges) = dependency_graph(&state);
            let cycles = cycle_members(&state);
            Json(serde_json::json!({
                "nodes": nodes
                    .iter()
                    .map(|n| serde_json::json!({
                        "id": n.id.as_str(),
                        "status": n.status,
                        "cycle": cycles.contains(&n.id),
                    }))
                    .collect::<Vec<_>>(),
                "edges": edges
                    .iter()
                    .map(|e| serde_json::json!({
                        "dependent": e.dependent.as_str(),
                        "prerequisite": e.prerequisite.as_str(),
                    }))
                    .collect::<Vec<_>>(),
            }))
            .into_response()
        }
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

/// Cycle-performance health overlay for the dashboard Overview panel (CXA-F018).
/// Same auth surface as `/metrics`: project membership via `auth_mw`.
pub(super) async fn metrics_summary_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match p.store.load().await {
        Ok(state) => {
            let today = now_rfc3339();
            let day = today.get(..10).unwrap_or("").to_owned();
            Json(coxagent_application::metrics_health::compute_cycle_perf(
                &state, &day,
            ))
            .into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Day-by-day time-series for line charts (AC3). `?days=N` bounds the window;
/// defaults to 14 when absent or unparsable.
pub(super) async fn metrics_trends_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let days = q
        .get("days")
        .and_then(|d| d.parse::<usize>().ok())
        .unwrap_or(14);
    match p.store.load().await {
        Ok(state) => {
            let today = now_rfc3339();
            let day = today.get(..10).unwrap_or("").to_owned();
            Json(coxagent_application::metrics_health::compute_trends(
                &state, days, &day,
            ))
            .into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Bug burn-down history (CXA-F032): per-day open/fixed/verified counts over
/// the dashboard window plus the net open-bug change across the last two
/// known days (`delta_24h`, positive = backlog burned down). Same auth surface
/// as the other `/metrics` reads: project membership via `auth_mw`.
pub(super) async fn metrics_burndown_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match p.store.load().await {
        Ok(state) => {
            let day = now_rfc3339().get(..10).unwrap_or("").to_owned();
            Json(coxagent_application::metrics::compute_burndown(
                &state,
                &day,
                coxagent_application::metrics::BURNDOWN_WINDOW_DAYS,
            ))
            .into_response()
        }
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

/// What the Settings screen should be shown for a config file that reads as
/// `text` (`None` = no file on disk). Pure, so the rule is testable without a
/// project fixture.
///
/// A project that never wrote a `coxagent.json` runs on defaults by design, so
/// that case is `Ok`. A file that IS there but holds a field the schema cannot
/// represent is refused, exactly as the runner's own load refuses it
/// (COX-B043) — see [`get_config`] for why answering with defaults is worse
/// here than anywhere else.
fn config_for_settings(
    text: Option<&str>,
) -> Result<Config, coxagent_application::config_parse::ConfigParseError> {
    text.map_or_else(
        || Ok(Config::default()),
        coxagent_application::config_parse::parse_config,
    )
}

/// The Settings screen's view of `coxagent.json`.
///
/// This handler must never answer a malformed file with `Config::default()`.
/// The screen is not read-only: [`put_config`] writes back the whole document
/// the screen was handed, and `saveSettings` deliberately carries the fields
/// the form does not render so they survive a save. Defaulting here therefore
/// turns one typo'd field into a full config wipe PERSISTED TO DISK the next
/// time anyone presses Save — including emptying `policy.model_allowlist` and
/// `policy.forbidden_paths`, i.e. silently turning the governance gates off.
/// That is the same wipe COX-B043 locked out of the runner's load path; the
/// admin UI is simply the other way in (COX-B050). Fail closed and name the
/// field instead, so the operator fixes the file rather than overwriting it.
pub(super) async fn get_config(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let text = std::fs::read_to_string(&p.config_path).ok();
    match config_for_settings(text.as_deref()) {
        Ok(cfg) => Json(cfg).into_response(),
        Err(e) => (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": e.to_string(),
                "field": e.field,
                "detail": e.detail,
            })),
        )
            .into_response(),
    }
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
mod settings_config_tests {
    use super::{config_for_settings, Config};

    /// A governed project whose config is healthy apart from the one field the
    /// caller injects — the shape that matters, because the damage is to the
    /// NEIGHBOURING settings, not to the bad field itself.
    fn governed(host_port: &str) -> String {
        let mut cfg = serde_json::to_value(Config::default()).expect("defaults as json");
        cfg["policy"]["model_allowlist"] = serde_json::json!(["claude/sonnet"]);
        cfg["policy"]["forbidden_paths"] = serde_json::json!(["infra/"]);
        cfg["deploy"]["host_port"] = serde_json::from_str(host_port).expect("port token is JSON");
        serde_json::to_string(&cfg).expect("config text")
    }

    /// AC (COX-B050): the Settings screen must not be handed a default config
    /// because one field is unreadable. It PUTs back what it was given, so
    /// defaults here become the operator's file on the next Save.
    #[test]
    fn a_malformed_field_is_refused_rather_than_shown_as_defaults() {
        let err = config_for_settings(Some(&governed("99999")))
            .expect_err("an out-of-range port must not be silently defaulted");

        assert_eq!(err.field, "deploy.host_port", "the field is named: {err}");
    }

    /// The specific harm, stated as its own case: answering with defaults
    /// empties the governance policy, and Save would then persist that —
    /// turning the model allowlist and forbidden paths off, unasked.
    #[test]
    fn the_governance_policy_is_never_quietly_emptied_by_a_bad_port() {
        let shown = config_for_settings(Some(&governed("99999"))).ok();

        assert!(
            shown.is_none(),
            "a config whose policy would come back empty must not be shown at all"
        );
    }

    /// A healthy file is shown exactly as written — failing closed must not
    /// become a licence to refuse working configs.
    #[test]
    fn a_healthy_config_is_shown_as_written() {
        let cfg = config_for_settings(Some(&governed("8101"))).expect("a valid config is shown");

        assert_eq!(cfg.deploy.host_port, Some(8101));
        assert_eq!(cfg.policy.model_allowlist, vec!["claude/sonnet".to_owned()]);
    }

    /// A project that never wrote a config runs on defaults by design; that is
    /// not a fault and must not become an error banner.
    #[test]
    fn a_missing_config_file_is_defaults_not_an_error() {
        let cfg = config_for_settings(None).expect("no file is not a fault");

        assert_eq!(cfg.deploy.host_port, None);
    }

    /// A file that is not JSON at all has no single field to blame, but it is
    /// still refused rather than defaulted.
    #[test]
    fn a_file_that_is_not_json_is_refused_too() {
        let err = config_for_settings(Some("{\"deploy\": ")).expect_err("truncated JSON");

        assert_eq!(
            err.field,
            coxagent_application::config_parse::WHOLE_DOCUMENT
        );
    }
}
