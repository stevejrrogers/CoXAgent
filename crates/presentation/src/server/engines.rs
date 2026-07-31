// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Engines and MCP: model lists, per-user tokens, evals, the MCP endpoint.

use super::*;

/// MCP server (streamable-HTTP JSON-RPC) — the gateway's third transport
/// beside REST and WS. Engines and MCP clients (claude CLI, Claude Desktop,
/// Cursor) PULL exactly the context they need instead of being fed capped
/// prompt blocks. Same authz as REST (session cookie or API token via the
/// auth middleware); every tools/call is audited.
pub(super) async fn mcp_ep(
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
pub(super) fn mcp_tool_specs() -> serde_json::Value {
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
pub(super) async fn mcp_call(
    app: &AppState,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
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

/// Agent CLIs detected on this machine's PATH — so the dashboard can show what
/// can actually run locally, not just the known engine types.
pub(super) async fn engines_ep(State(app): State<AppState>) -> impl IntoResponse {
    let list: Vec<_> = app
        .engines
        .iter()
        .map(|(name, path)| serde_json::json!({ "name": name, "path": path }))
        .collect();
    Json(list)
}

/// Detect opencode models from the live CLI: runs `opencode models`, parses
/// output into provider/model pairs. Returns empty list on any failure.
pub(super) async fn opencode_models_ep() -> impl IntoResponse {
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

/// The provider + base URL configured for a project's git integration.
/// Whether the token-saver is enabled for a project (default on).
pub(super) fn project_token_saver(p: &ProjectHandle) -> bool {
    std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .map_or(true, |c| c.workflow.token_saver)
}

/// Deterministic per-role performance + team quality stats for the Agents view.
pub(super) async fn agent_evals_ep(
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

/// Token-saver effectiveness: aggregate the shim compression log (bytes before
/// vs after) into a headline "how much did we save" for the Cost view.
#[allow(clippy::cast_precision_loss)]
pub(super) async fn token_saver_ep() -> axum::response::Response {
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

/// A collision-free token for stored media filenames (nanos + a counter).
pub(super) fn mint_media_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}{seq:x}")
}

/// The `Authorization: Bearer <token>` value, if present.
pub(super) fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ").map(|t| t.trim().to_owned())
}

/// Mint an API token for a service account. The secret is returned once and
/// never stored in plaintext. Admin-only (enforced by the middleware).
pub(super) async fn create_token_ep(
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
pub(super) async fn list_tokens_ep(
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
pub(super) fn personal_token_prefix(username: &str) -> String {
    format!("user:{}:", username.to_ascii_lowercase())
}

/// List the caller's own personal tokens (metadata only). Any signed-in user.
pub(super) async fn my_tokens_ep(
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

/// Mint a personal API token bound to the caller's own account and role.
/// Any signed-in user; the secret is returned once and never stored.
pub(super) async fn create_my_token_ep(
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
pub(super) async fn revoke_my_token_ep(
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
pub(super) async fn revoke_token_ep(
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
