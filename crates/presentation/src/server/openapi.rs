//! Self-describing API: serve an OpenAPI 3.x document describing every service
//! route so MCP clients and SDK generators can discover endpoints without
//! reading Rust source (CXA-F023 / CXA-B051).
//!
//! Axum 0.7 exposes no practical runtime route introspection, so this module
//! keeps ONE structured table - [`ROUTES`] - mirroring what `serve_full()`
//! chains onto its `Router::new()` (`crates/presentation/src/server/mod.rs`).
//! [`build_document`] is a pure function over that table; nothing here depends
//! on request context. The drift guard in `crates/app/tests/openapi_routes_gate.rs`
//! fails CI if a registered `/api/*` path is missing from [`ROUTES`] (or the
//! reverse), so the mirror cannot rot silently.
use super::*;

/// One service route as declared on axum's router chain.
#[derive(Debug, Clone)]
pub(crate) struct RouteSpec {
    pub path: &'static str,
    pub methods: &'static [&'static str],
}

const fn route(path: &'static str, methods: &'static [&'static str]) -> RouteSpec {
    RouteSpec { path, methods }
}

/// Every service route registered by `serve_full()`. Keep this complete:
/// `openapi_routes_gate.rs` fails CI if any registered `/api/*` path is missing.
pub(crate) const ROUTES: &[RouteSpec] = &[
    route("/api/analyze-goal", &["post"]),
    route("/api/app/download/:file", &["get"]),
    route("/api/app/latest", &["get"]),
    route("/api/audit-log", &["get"]),
    route("/api/auth/2fa/disable", &["post"]),
    route("/api/auth/2fa/enable", &["post"]),
    route("/api/auth/2fa/enroll", &["post"]),
    route("/api/auth/login", &["post"]),
    route("/api/auth/logout", &["post"]),
    route("/api/auth/me", &["get"]),
    route("/api/auth/my/tokens", &["get", "post"]),
    route("/api/auth/my/tokens/:label", &["delete"]),
    route("/api/auth/profile", &["patch"]),
    route("/api/auth/sessions", &["get"]),
    route("/api/auth/tokens", &["get", "post"]),
    route("/api/auth/tokens/:label", &["delete"]),
    route("/api/auth/users", &["get", "post"]),
    route("/api/auth/users/:username", &["delete", "patch"]),
    route("/api/auth/users/:username/password", &["post"]),
    route("/api/chat/channel/:cid/topic", &["get", "patch"]),
    route("/api/chat/channels", &["get", "post"]),
    route("/api/chat/channels/:cid", &["delete"]),
    route("/api/chat/channels/:cid/invite", &["post"]),
    route("/api/chat/channels/:cid/members/:member", &["delete"]),
    route("/api/chat/channels/:cid/settings", &["patch"]),
    route("/api/chat/channels/:cid/topic", &["get", "patch"]),
    route("/api/chat/dm", &["post"]),
    route("/api/chat/hook/:token", &["post"]),
    route("/api/chat/ice", &["get"]),
    route("/api/chat/media/:file", &["get"]),
    route("/api/chat/members", &["get"]),
    route("/api/chat/messages", &["get"]),
    route("/api/chat/messages/:mid", &["delete", "patch"]),
    route("/api/chat/messages/:mid/pin", &["post"]),
    route("/api/chat/messages/:mid/reply", &["post"]),
    route("/api/chat/messages/:mid/thread", &["get"]),
    route("/api/chat/pins", &["get"]),
    route("/api/chat/react", &["post"]),
    route("/api/chat/search", &["get"]),
    route("/api/chat/send", &["post"]),
    route("/api/chat/upload", &["post"]),
    route("/api/chat/webhooks", &["get", "post"]),
    route("/api/chat/webhooks/:token", &["delete"]),
    route("/api/chat/ws", &["get"]),
    route("/api/engines", &["get"]),
    route("/api/engines/opencode/models", &["get"]),
    route("/api/health", &["get"]),
    route("/api/manage/overview", &["get"]),
    route("/api/manage/spaces/:sid", &["get"]),
    route("/api/mcp", &["post"]),
    route("/api/me/agents", &["get"]),
    route("/api/meetings", &["get", "post"]),
    route("/api/meetings/:id", &["patch"]),
    route("/api/meetings/:id/join", &["post"]),
    route("/api/meetings/:id/ring", &["post"]),
    route("/api/openapi.json", &["get"]),
    route("/api/people-analytics", &["get"]),
    route("/api/pr-report", &["post"]),
    route("/api/pr-report/reviews", &["get"]),
    route("/api/profile", &["post"]),
    route("/api/profile/avatar", &["delete", "post"]),
    route("/api/profiles", &["get"]),
    route("/api/projects", &["get", "post"]),
    route("/api/projects/:pid", &["delete", "patch"]),
    route("/api/projects/:pid/agent-evals", &["get"]),
    route("/api/projects/:pid/agent-log", &["get"]),
    route("/api/projects/:pid/agent-log/stream", &["get"]),
    route("/api/projects/:pid/architecture-review", &["post"]),
    route("/api/projects/:pid/attachment", &["get"]),
    route("/api/projects/:pid/audit", &["get"]),
    route("/api/projects/:pid/ba-analyze", &["post"]),
    route("/api/projects/:pid/channels", &["get", "post"]),
    route("/api/projects/:pid/channels/:cid/invite", &["post"]),
    route(
        "/api/projects/:pid/channels/:cid/members/:member",
        &["delete"],
    ),
    route("/api/projects/:pid/channels/:cid/settings", &["patch"]),
    route("/api/projects/:pid/chat", &["get", "post"]),
    route("/api/projects/:pid/chat-reply", &["post"]),
    route("/api/projects/:pid/chat/ws", &["get"]),
    route("/api/projects/:pid/codegraph", &["get"]),
    route("/api/projects/:pid/codegraph/build", &["post"]),
    route("/api/projects/:pid/codegraph/deps", &["get"]),
    route("/api/projects/:pid/codegraph/refs", &["get"]),
    route("/api/projects/:pid/comments", &["get", "post"]),
    route("/api/projects/:pid/comments/:id/react", &["post"]),
    route("/api/projects/:pid/config", &["get", "put"]),
    route("/api/projects/:pid/context", &["get", "post"]),
    route("/api/projects/:pid/control/:action", &["post"]),
    route("/api/projects/:pid/digest", &["post"]),
    route("/api/projects/:pid/discuss", &["post"]),
    route("/api/projects/:pid/doc-folders", &["get", "post"]),
    route("/api/projects/:pid/doc-folders/delete", &["post"]),
    route("/api/projects/:pid/docs", &["get"]),
    route("/api/projects/:pid/docs-review", &["post"]),
    route("/api/projects/:pid/docs/:id", &["delete", "put"]),
    route("/api/projects/:pid/docs/:id/ai-edit", &["post"]),
    route("/api/projects/:pid/docs/:id/move", &["post"]),
    route("/api/projects/:pid/docs/:id/ws", &["get"]),
    route("/api/projects/:pid/docs/generate", &["post"]),
    route("/api/projects/:pid/events", &["get"]),
    route("/api/projects/:pid/file", &["get"]),
    route("/api/projects/:pid/git/auth", &["get"]),
    route("/api/projects/:pid/git/connect", &["post"]),
    route("/api/projects/:pid/git/test", &["post"]),
    route("/api/projects/:pid/inbox", &["get"]),
    route("/api/projects/:pid/media/:file", &["get"]),
    route("/api/projects/:pid/members", &["get", "post"]),
    route("/api/projects/:pid/members/:username", &["delete"]),
    route("/api/projects/:pid/merge-sweep", &["post"]),
    route("/api/projects/:pid/metrics", &["get"]),
    route("/api/projects/:pid/metrics/summary", &["get"]),
    route("/api/projects/:pid/metrics/trends", &["get"]),
    route("/api/projects/:pid/operators/:operator/:action", &["post"]),
    route("/api/projects/:pid/pr/:number/human", &["post"]),
    route("/api/projects/:pid/prs", &["get"]),
    route("/api/projects/:pid/prs/:num/:action", &["post"]),
    route("/api/projects/:pid/prs/:num/diff", &["get"]),
    route("/api/projects/:pid/runner", &["get"]),
    route("/api/projects/:pid/sprint/:action", &["post"]),
    route("/api/projects/:pid/sprint/close", &["post"]),
    route("/api/projects/:pid/sprint/goal", &["post"]),
    route("/api/projects/:pid/standup", &["post"]),
    route("/api/projects/:pid/state", &["get"]),
    route("/api/projects/:pid/store", &["post"]),
    route("/api/projects/:pid/terminal", &["get"]),
    route("/api/projects/:pid/ticket-refine", &["post"]),
    route("/api/projects/:pid/ticket/:id", &["get"]),
    route("/api/projects/:pid/ticket/:id/approve-cost", &["post"]),
    route("/api/projects/:pid/ticket/:id/assign", &["post"]),
    route("/api/projects/:pid/ticket/:id/attachments", &["post"]),
    route("/api/projects/:pid/ticket/:id/edit", &["post"]),
    route("/api/projects/:pid/ticket/:id/pre-mortem", &["post"]),
    route("/api/projects/:pid/ticket/:id/priority", &["post"]),
    route("/api/projects/:pid/ticket/:id/ready", &["post"]),
    route("/api/projects/:pid/ticket/:id/reject", &["post"]),
    route("/api/projects/:pid/ticket/:id/send-back", &["post"]),
    route("/api/projects/:pid/ticket/:id/undo-approval", &["post"]),
    route("/api/projects/:pid/ticket/:id/unpark", &["post"]),
    route("/api/projects/:pid/ticket/:id/verify", &["post"]),
    route("/api/projects/:pid/tickets", &["post"]),
    route("/api/projects/:pid/transcripts", &["get"]),
    route("/api/projects/:pid/transcripts/:name", &["get"]),
    route("/api/projects/:pid/upload", &["post"]),
    route("/api/projects/:pid/workers", &["get"]),
    route("/api/projects/:pid/workspace", &["get"]),
    route("/api/spaces", &["get", "post"]),
    route("/api/spaces/:sid", &["delete", "put"]),
    route("/api/token-saver", &["get"]),
    route("/api/tooling", &["get"]),
    route("/api/workspace", &["get", "put"]),
    route("/api/workspace/invites", &["get", "post"]),
    route("/api/workspace/invites/:token", &["delete"]),
    route("/api/workspace/join", &["post"]),
    route("/api/workspace/overview", &["get"]),
];

/// Serve the OpenAPI document at `/api/openapi.json`.
pub(super) async fn openapi_ep() -> Json<serde_json::Value> {
    Json(build_document())
}

/// Build an OpenAPI 3.x document from [`ROUTES`]. Pure and dependency-free so it
/// is trivially testable; every call recomputes from the single table.
fn build_document() -> serde_json::Value {
    let mut paths = std::collections::BTreeMap::new();
    for spec in ROUTES {
        let item = paths
            .entry(spec.path)
            .or_insert_with(std::collections::BTreeMap::new);
        for method in spec.methods {
            item.insert((*method).to_owned(), operation(spec.path, method));
        }
    }
    serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "CoXAgent Hub API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST API of the CoXAgent hub dashboard.",
        },
        // BTreeMap serialises keys sorted, keeping output stable across runs.
        "paths": paths,
    })
}

fn operation(path: &str, method: &str) -> serde_json::Value {
    serde_json::json!({
        // Unique per verb - a multi-method route yields one id per method, so ids
        // never collide (SDK generators index operations by id).
        "operationId": format!("{}_{}", method.to_lowercase(), tail_word(path)),
        // Parameters come from :path segments; bodies are not modelled here.
        "parameters": parameters(path),
        // Synthetic response contract - request/response bodies are not modelled,
        // so only an empty description ships.
        "responses": { "200": { "description": "" } },
    })
}

/// Path parameters derived from `:segment`s - one required string param each,
/// emitted in path order as an OpenAPI Parameter array.
fn parameters(path: &str) -> Vec<serde_json::Value> {
    path.split('/')
        .filter_map(|seg| seg.strip_prefix(':'))
        .map(|name| {
            serde_json::json!({
                "name": name,
                // Every named segment is a required path parameter; hub routes treat
                // ids as strings, so no type inference is attempted here.
                "in": "path",
                "required": true,
                "schema": { "type": "string" },
            })
        })
        .collect()
}

/// Stable snake-case operation id built from static path segments; `:param`s are
/// stripped to their bare name and joined with underscores.
fn tail_word(path: &str) -> String {
    path.trim_start_matches('/')
        .split('/')
        .map(|seg| seg.trim_start_matches(':').replace(['-', ':'], "_"))
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colon_segments_become_named_path_parameters_in_order() {
        let params = parameters("/api/projects/:pid/tickets/:id");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0]["name"], json!("pid"));
        assert_eq!(params[1]["name"], json!("id"));
        assert!(params.iter().all(|p| p["required"] == json!(true)));
        assert!(params.iter().all(|p| p["in"] == json!("path")));
    }

    #[test]
    fn static_routes_have_no_parameters() {
        assert!(parameters("/api/health").is_empty());
    }

    #[test]
    fn tail_word_is_snake_case_from_static_segments() {
        assert_eq!(
            tail_word("/api/projects/:pid/ticket/:id/priority"),
            "api_projects_pid_ticket_id_priority"
        );
    }

    #[test]
    fn every_route_and_method_from_the_table_is_in_the_document() {
        let doc = build_document();
        let paths = doc["paths"].as_object().expect("paths object");
        // One Path Item per distinct route...
        assert_eq!(paths.len(), ROUTES.len());
        for spec in ROUTES {
            let item = paths[spec.path]
                .as_object()
                .unwrap_or_else(|| panic!("missing path item for {}", spec.path));
            // ...and one operation per supported method on that path.
            for method in spec.methods {
                assert!(
                    item.contains_key(*method),
                    "missing {method} operation for {}",
                    spec.path
                );
            }
        }
    }

    #[test]
    fn operation_ids_are_unique_across_the_whole_document() {
        // SDK generators index operations by id, so a collision (e.g. two routes
        // whose tails collapse after stripping :params) breaks generated clients.
        let doc = build_document();
        let mut seen = std::collections::BTreeSet::new();
        for item in doc["paths"].as_object().unwrap().values() {
            for op in item.as_object().unwrap().values() {
                let id = op["operationId"].as_str().expect("operationId string");
                assert!(
                    seen.insert(id.to_string()),
                    "duplicate OpenAPI operationId: {id}"
                );
            }
        }
    }
}
