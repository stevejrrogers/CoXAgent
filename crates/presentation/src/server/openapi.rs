//! Self-describing API: an OpenAPI 3.x document for every service route the hub
//! registers in `serve_full()` (CXA-F023).
//!
//! The hub ships no OpenAPI dependency and axum v0 exposes no practical route
//! introspection we can lean on without restructuring how `serve_full()` builds
//! its router. So this module keeps ONE structured table — [`ROUTES`] —
//! mirroring exactly what `serve_full()` chains onto its `Router::new()`
//! (`crates/presentation/src/server/mod.rs`, ~line 581-883). [`build_document`]
//! is a pure function over that table, so it cannot depend on request context.
use super::*;

/// One service route as declared on axum's router chain.
#[derive(Debug, Clone)]
pub(crate) struct RouteSpec {
    pub path: &'static str,
    pub methods: &'static [&'static str],
    pub tag: &'static str,
}

const fn route(
    path: &'static str,
    methods: &'static [&'static str],
    tag: &'static str,
) -> RouteSpec {
    RouteSpec { path, methods, tag }
}

/// Every service route registered by `serve_full()`. Keep this mirror complete:
/// `openapi_routes_gate.rs` fails CI if a registered `/api/*` path is missing here.
pub(crate) const ROUTES: &[RouteSpec] = &[
    route("/api/health", &["get"], "Hub"),
    route("/api/openapi.json", &["get"], "Hub"),
    route("/api/mcp", &["post"], "System"),
    route("/api/app/latest", &["get"], "App"),
    route("/api/app/download/:file", &["get"], "App"),
    route("/api/auth/login", &["post"], "Auth"),
    route("/api/auth/logout", &["post"], "Auth"),
    route("/api/auth/me", &["get"], "Auth"),
    route("/api/auth/sessions", &["get"], "Auth"),
    route("/api/auth/tokens", &["get", "post"], "Auth"),
    route("/api/auth/tokens/:label", &["delete"], "Auth"),
    route("/api/auth/my/tokens", &["get", "post"], "Auth"),
    route("/api/meetings", &["get", "post"], "Meetings"),
    route("/api/meetings/:id", &["patch"], "Meetings"),
    route("/api/meetings/:id/join", &["post"], "Meetings"),
    route("/api/meetings/:id/ring", &["post"], "Meetings"),
    route("/api/profiles", &["get"], "Profile"),
    route("/api/profile", &["post"], "Profile"),
    route("/api/auth/profile", &["patch"], "Auth"),
    route("/api/profile/avatar", &["post", "delete"], "Profile"),
    route("/api/auth/my/tokens/:label", &["delete"], "Auth"),
    route("/api/auth/2fa/enroll", &["post"], "Auth"),
    route("/api/auth/2fa/enable", &["post"], "Auth"),
    route("/api/auth/2fa/disable", &["post"], "Auth"),
    route("/api/auth/users", &["get", "post"], "Auth"),
    route("/api/auth/users/:username", &["patch", "delete"], "Auth"),
    route("/api/auth/users/:username/password", &["post"], "Auth"),
    route("/api/audit-log", &["get"], "Audit"),
    route("/api/people-analytics", &["get"], "People"),
    route("/api/projects/:pid/workspace", &["get"], "Projects"),
    route("/api/projects/:pid/file", &["get"], "Projects"),
    route("/api/projects/:pid/members", &["get", "post"], "Projects"),
    route(
        "/api/projects/:pid/members/:username",
        &["delete"],
        "Projects",
    ),
    route("/api/chat/channels", &["get", "post"], "Chat"),
    route("/api/chat/channels/:cid", &["delete"], "Chat"),
    route("/api/chat/channels/:cid/invite", &["post"], "Chat"),
    route("/api/chat/channels/:cid/settings", &["patch"], "Chat"),
    route(
        "/api/chat/channels/:cid/members/:member",
        &["delete"],
        "Chat",
    ),
    route("/api/chat/channel/:cid/topic", &["get", "patch"], "Chat"),
    route("/api/chat/channels/:cid/topic", &["get", "patch"], "Chat"),
    route("/api/chat/messages", &["get"], "Chat"),
    route("/api/chat/members", &["get"], "Chat"),
    route("/api/chat/dm", &["post"], "Chat"),
    route("/api/chat/react", &["post"], "Chat"),
    route("/api/chat/messages/:mid/reply", &["post"], "Chat"),
    route("/api/chat/messages/:mid/thread", &["get"], "Chat"),
    route("/api/chat/messages/:mid", &["patch", "delete"], "Chat"),
    route("/api/chat/search", &["get"], "Chat"),
    route("/api/chat/messages/:mid/pin", &["post"], "Chat"),
    route("/api/chat/pins", &["get"], "Chat"),
    route("/api/chat/webhooks", &["get", "post"], "Chat"),
    route("/api/chat/webhooks/:token", &["delete"], "Chat"),
    route("/api/chat/hook/:token", &["post"], "Chat"),
    route("/api/chat/ice", &["get"], "Chat"),
    route("/api/chat/send", &["post"], "Chat"),
    route("/api/chat/ws", &["get"], "Chat"),
    route("/api/chat/upload", &["post"], "Chat"),
    route("/api/chat/media/:file", &["get"], "Chat"),
    route("/api/engines", &["get"], "System"),
    route("/api/engines/opencode/models", &["get"], "System"),
    route("/api/tooling", &["get"], "System"),
    route("/api/analyze-goal", &["post"], "Hub"),
    route("/api/projects", &["get", "post"], "Projects"),
    route("/api/projects/:pid", &["patch", "delete"], "Projects"),
    route("/api/projects/:pid/store", &["post"], "Projects"),
    route("/api/projects/:pid/state", &["get"], "Projects"),
    route("/api/projects/:pid/metrics", &["get"], "Projects"),
    route("/api/projects/:pid/agent-evals", &["get"], "Projects"),
    route("/api/projects/:pid/runner", &["get"], "Projects"),
    route("/api/projects/:pid/workers", &["get"], "Projects"),
    route("/api/token-saver", &["get"], "System"),
    route("/api/projects/:pid/audit", &["get"], "Projects"),
    route("/api/projects/:pid/config", &["get", "put"], "Projects"),
    route("/api/projects/:pid/control/:action", &["post"], "Projects"),
    route("/api/projects/:pid/sprint/goal", &["post"], "Projects"),
    route("/api/projects/:pid/sprint/close", &["post"], "Projects"),
    route("/api/projects/:pid/sprint/:action", &["post"], "Projects"),
    route("/api/projects/:pid/digest", &["post"], "Projects"),
    route("/api/projects/:pid/merge-sweep", &["post"], "Projects"),
    route("/api/workspace", &["get", "put"], "Workspace"),
    route("/api/workspace/invites", &["get", "post"], "Workspace"),
    route("/api/workspace/invites/:token", &["delete"], "Workspace"),
    route("/api/workspace/overview", &["get"], "Workspace"),
    route("/api/spaces", &["get", "post"], "Spaces"),
    route("/api/spaces/:sid", &["put", "delete"], "Spaces"),
    route("/api/manage/overview", &["get"], "Manage"),
    route("/api/manage/spaces/:sid", &["get"], "Manage"),
    route("/api/me/agents", &["get"], "Me"),
    route("/api/workspace/join", &["post"], "Workspace"),
    route(
        "/api/projects/:pid/operators/:operator/:action",
        &["post"],
        "Projects",
    ),
    route("/api/projects/:pid/ba-analyze", &["post"], "Projects"),
    route("/api/projects/:pid/ticket-refine", &["post"], "Projects"),
    route("/api/projects/:pid/discuss", &["post"], "Projects"),
    route("/api/projects/:pid/docs", &["get"], "Projects"),
    route("/api/projects/:pid/docs/generate", &["post"], "Projects"),
    route(
        "/api/projects/:pid/doc-folders",
        &["get", "post"],
        "Projects",
    ),
    route(
        "/api/projects/:pid/doc-folders/delete",
        &["post"],
        "Projects",
    ),
    route("/api/projects/:pid/docs/:id/move", &["post"], "Projects"),
    route(
        "/api/projects/:pid/docs/:id",
        &["put", "delete"],
        "Projects",
    ),
    route("/api/projects/:pid/docs/:id/ai-edit", &["post"], "Projects"),
    route("/api/projects/:pid/docs/:id/ws", &["get"], "Projects"),
    route("/api/projects/:pid/terminal", &["get"], "Projects"),
    route("/api/projects/:pid/codegraph", &["get"], "Projects"),
    route("/api/projects/:pid/codegraph/refs", &["get"], "Projects"),
    route("/api/projects/:pid/codegraph/deps", &["get"], "Projects"),
    route("/api/projects/:pid/codegraph/build", &["post"], "Projects"),
    route("/api/projects/:pid/standup", &["post"], "Projects"),
    route(
        "/api/projects/:pid/architecture-review",
        &["post"],
        "Projects",
    ),
    route("/api/projects/:pid/docs-review", &["post"], "Projects"),
    route("/api/projects/:pid/chat-reply", &["post"], "Projects"),
    route("/api/projects/:pid/tickets", &["post"], "Projects"),
    route("/api/projects/:pid/ticket/:id", &["get"], "Projects"),
    route(
        "/api/projects/:pid/ticket/:id/priority",
        &["post"],
        "Projects",
    ),
    route(
        "/api/projects/:pid/ticket/:id/reject",
        &["post"],
        "Projects",
    ),
    route(
        "/api/projects/:pid/ticket/:id/approve-cost",
        &["post"],
        "Projects",
    ),
    route("/api/projects/:pid/inbox", &["get"], "Projects"),
    route("/api/projects/:pid/pr/:number/human", &["post"], "Projects"),
    route("/api/projects/:pid/attachment", &["get"], "Projects"),
    route(
        "/api/projects/:pid/ticket/:id/attachments",
        &["post"],
        "Projects",
    ),
    route("/api/projects/:pid/ticket/:id/ready", &["post"], "Projects"),
    route(
        "/api/projects/:pid/ticket/:id/verify",
        &["post"],
        "Projects",
    ),
    route(
        "/api/projects/:pid/ticket/:id/send-back",
        &["post"],
        "Projects",
    ),
    route(
        "/api/projects/:pid/ticket/:id/assign",
        &["post"],
        "Projects",
    ),
    route(
        "/api/projects/:pid/ticket/:id/undo-approval",
        &["post"],
        "Projects",
    ),
    route(
        "/api/projects/:pid/ticket/:id/unpark",
        &["post"],
        "Projects",
    ),
    route("/api/projects/:pid/ticket/:id/edit", &["post"], "Projects"),
    route("/api/projects/:pid/comments", &["get", "post"], "Projects"),
    route(
        "/api/projects/:pid/comments/:id/react",
        &["post"],
        "Projects",
    ),
    route("/api/projects/:pid/chat", &["get", "post"], "Projects"),
    route("/api/projects/:pid/chat/ws", &["get"], "Projects"),
    route("/api/projects/:pid/channels", &["get", "post"], "Projects"),
    route(
        "/api/projects/:pid/channels/:cid/settings",
        &["patch"],
        "Projects",
    ),
    route(
        "/api/projects/:pid/channels/:cid/members/:member",
        &["delete"],
        "Projects",
    ),
    route(
        "/api/projects/:pid/channels/:cid/invite",
        &["post"],
        "Projects",
    ),
    route("/api/projects/:pid/upload", &["post"], "Projects"),
    route("/api/projects/:pid/media/:file", &["get"], "Projects"),
    route("/api/projects/:pid/context", &["get", "post"], "Projects"),
    route("/api/projects/:pid/git/auth", &["get"], "Projects"),
    route("/api/projects/:pid/git/connect", &["post"], "Projects"),
    route("/api/projects/:pid/git/test", &["post"], "Projects"),
    route("/api/projects/:pid/prs", &["get"], "Projects"),
    route("/api/pr-report", &["post"], "Hub"),
    route("/api/pr-report/reviews", &["get"], "Hub"),
    route("/api/projects/:pid/prs/:num/diff", &["get"], "Projects"),
    route("/api/projects/:pid/prs/:num/:action", &["post"], "Projects"),
    route("/api/projects/:pid/agent-log", &["get"], "Projects"),
    route("/api/projects/:pid/agent-log/stream", &["get"], "Projects"),
    route("/api/projects/:pid/transcripts", &["get"], "Projects"),
    route("/api/projects/:pid/transcripts/:name", &["get"], "Projects"),
    route("/api/projects/:pid/events", &["get"], "Projects"),
];

/// The handler for GET /api/openapi.json: always answers 200 with the spec
/// document as JSON. Public like health — it is machine-readable discovery
/// metadata that SDK generators and MCP clients fetch without a session.
pub(super) async fn openapi_ep() -> impl IntoResponse {
    Json(build_document())
}

/// The canonical OpenAPI document, built from [`ROUTES`]. Pure over the table,
/// so repeated calls return identical output for the same set of routes.
fn build_document() -> serde_json::Value {
    build_document_from(ROUTES)
}

/// The pure core of [`build_document`], factored to take routes as input so unit
/// tests can feed arbitrary fixtures instead of the module-global table.
fn build_document_from(specs: &[RouteSpec]) -> serde_json::Value {
    // Group operations under each path template. A BTreeMap gives stable, sorted
    // output and needs no panicking accessors — every insert goes through the
    // entry API.
    let mut grouped: std::collections::BTreeMap<&str, serde_json::Map<String, serde_json::Value>> =
        std::collections::BTreeMap::new();
    for spec in specs {
        for method in spec.methods {
            grouped
                .entry(spec.path)
                .or_default()
                .insert((*method).to_owned(), operation(spec, method));
        }
    }
    let paths: serde_json::Map<String, serde_json::Value> = grouped
        .into_iter()
        .map(|(path, ops)| (path.to_owned(), serde_json::Value::Object(ops)))
        .collect();
    serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "CoXAgent Hub API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST API of the CoXAgent hub dashboard.",
        },
        "paths": paths,
    })
}

fn operation(spec: &RouteSpec, method: &str) -> serde_json::Value {
    serde_json::json!({
        "tags": [spec.tag],
        "summary": format!("{} {}", method.to_uppercase(), spec.path),
        // Unique per verb — a multi-method route yields one operation per method,
        // so ids must not collide across them (SDK generators rely on uniqueness).
        "operationId": format!("{}_{}", method, tail_word(spec.path)),
        "parameters": parameters(spec.path),
        // Synthetic response contract; every endpoint today returns JSON. Bodies
        // are not modelled (see boundary #3) so only an empty description ships.
        "responses": { "200": { "description": "" } },
    })
}

/// The path portion of an operation id: slash-separated segments joined with
/// underscores, colon params stripped to their bare name.
fn tail_word(path: &str) -> String {
    path.trim_start_matches('/')
        .split('/')
        .map(|seg| seg.trim_start_matches(':').replace(['-', ':'], "_"))
        .collect::<Vec<_>>()
        .join("_")
}
fn parameters(path: &str) -> Vec<serde_json::Value> {
    path.split('/')
        .filter_map(|seg| seg.strip_prefix(':'))
        .map(|name| {
            serde_json::json!({
                "name": name,
                // Every named segment is a required path parameter; hub routes
                // treat ids as strings, so no type inference is attempted here.
                "in": "path",
                "required": true,
                "schema": { "type": "string" },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colon_segments_become_named_path_parameters() {
        let params = parameters("/api/projects/:pid/tickets/:id");
        let names: Vec<_> = params.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["pid", "id"]);
        assert!(params
            .iter()
            .all(|p| p["required"] == serde_json::json!(true)));
        assert!(params.iter().all(|p| p["in"] == "path"));
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
    fn multi_method_routes_yield_distinct_operation_ids_per_method() {
        let fixture = [
            route("/api/auth/users", &["get", "post"], "Auth"),
            route("/api/auth/login", &["post"], "Auth"),
        ];
        let doc = build_document_from(&fixture);
        let paths = doc["paths"].as_object().unwrap();
        // Distinct paths each get their own Path Item, methods fold into one.
        assert_eq!(paths.len(), 2);

        // Every emitted operation id must be unique across the whole document —
        // SDK generators index operations by id. This is the regression guard
        // for the original bug where get+post on one path shared a single id.
        let mut ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for item in paths.values() {
            for op in item.as_object().unwrap().values() {
                let id = op["operationId"].as_str().expect("operationId string");
                assert!(ids.insert(id.to_string()), "duplicate operationId {id}");
            }
        }
    }

    #[test]
    fn document_shape_is_openapi_3_with_paths_map() {
        let doc = build_document();
        assert_eq!(doc["openapi"], "3.0.3");
        assert_eq!(doc["info"]["title"], "CoXAgent Hub API");
        // At least one path from our table must be present.
        let paths = doc["paths"].as_object().expect("paths object");
        assert!(!paths.is_empty());
    }
}
