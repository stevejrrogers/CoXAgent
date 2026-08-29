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

/// Access tier each operation requires — mirrors exactly what
/// [`super::auth::auth_mw`](auth_mw) enforces at runtime (see it for ground truth):
/// public paths pass through with no session; manage surfaces need an Admin/lead
/// role; everything else needs any valid session plus per-project membership.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SecurityLevel {
    /// No session required (health, login, openapi spec itself).
    Public,
    /// Any valid authenticated session (+ per-project membership where a pid is in path).
    Authenticated,
    /// User administration / token management surfaces limited to Admin+ lead roles.
    Admin,
}

impl SecurityLevel {
    /// Map to an OpenAPI security requirement array for one operation:
    /// public operations declare no requirement; protected ones require bearerAuth.
    fn requirement(self) -> serde_json::Value {
        if self == SecurityLevel::Public {
            return json!([]);
        }
        json!([{ "bearerAuth": [] }])
    }
}

/// One service route as declared on axum's router chain plus enriched OpenAPI
/// metadata. Tags/summaries default from stable path conventions ([autotag], [autosummary])
/// but may be overridden per entry where naming alone cannot express intent; security is always derived from real auth enforcement ([security_for]).
#[derive(Debug, Clone)]
pub(crate) struct RouteSpec {
    pub path: &'static str,
    pub methods: &'static [&'static str],
    pub tag: Option<&'static str>,
    pub summary: Option<&'static str>,
}

const fn route(path: &'static str, methods: &'static [&'static str]) -> RouteSpec {
    RouteSpec {
        path,
        methods,
        tag: None,
        summary: None,
    }
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
    route("/api/fleet/river", &["get"]),
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
    route("/api/projects/:pid/burn-mode", &["post"]),
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
    route("/api/projects/:pid/goals", &["post"]),
    route("/api/projects/:pid/goals/:gid/rename", &["post"]),
    route("/api/projects/:pid/goals/outcomes", &["get"]),
    route("/api/projects/:pid/inbox", &["get"]),
    route("/api/projects/:pid/media/:file", &["get"]),
    route("/api/projects/:pid/members", &["get", "post"]),
    route("/api/projects/:pid/members/:username", &["delete"]),
    route("/api/projects/:pid/merge-sweep", &["post"]),
    route("/api/projects/:pid/metrics", &["get"]),
    route("/api/projects/:pid/metrics/burndown", &["get"]),
    route("/api/projects/:pid/metrics/summary", &["get"]),
    route("/api/projects/:pid/metrics/trends", &["get"]),
    route("/api/projects/:pid/operators/:operator/:action", &["post"]),
    route("/api/projects/:pid/pr/:number/human", &["post"]),
    route("/api/projects/:pid/prs", &["get"]),
    route("/api/projects/:pid/prs/:num/:action", &["post"]),
    route("/api/projects/:pid/prs/:num/diff", &["get"]),
    route("/api/projects/:pid/reverts/:sha", &["post"]),
    route("/api/projects/:pid/runner", &["get"]),
    route("/api/projects/:pid/share-links", &["get", "post"]),
    route("/api/projects/:pid/share-links/:token", &["delete"]),
    route("/api/projects/:pid/sprint/:action", &["post"]),
    route("/api/projects/:pid/sprint/close", &["post"]),
    route("/api/projects/:pid/sprint/goal", &["post"]),
    route("/api/projects/:pid/ticket/:id/status/:action", &["post"]),
    route("/api/projects/:pid/sprint-queue", &["post"]),
    route("/api/projects/:pid/sprint-queue/:qid/scope", &["post"]),
    route("/api/projects/:pid/sprint-queue/:qid/rename", &["post"]),
    route("/api/projects/:pid/sprint-queue/:qid/move/:dir", &["post"]),
    route("/api/projects/:pid/sprint-queue/:qid", &["delete"]),
    route("/api/projects/:pid/standup", &["post"]),
    route("/api/projects/:pid/state", &["get"]),
    route("/api/projects/:pid/store", &["get", "post"]),
    route("/api/projects/:pid/terminal", &["get"]),
    route("/api/projects/:pid/ticket-refine", &["post"]),
    route("/api/projects/:pid/ticket/:id", &["get"]),
    route("/api/projects/:pid/ticket/:id/approve-cost", &["post"]),
    route("/api/projects/:pid/ticket/:id/assign", &["post"]),
    route("/api/projects/:pid/ticket/:id/attachments", &["post"]),
    route("/api/projects/:pid/ticket/:id/edit", &["post"]),
    route("/api/projects/:pid/ticket/:id/goal", &["post"]),
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
    route("/s/:token", &["get"]),
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
            item.insert((*method).to_owned(), operation(spec, method));
        }
    }
    serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "CoXAgent Hub API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST API of the CoXAgent hub dashboard.",
        },
        // The hub authenticates with a personal token via
        // `Authorization: Bearer <token>` (guards.rs) or a session cookie; only
        // the bearer scheme is declared so SDK generators can wire clients.
        // Public operations reference none of it.
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    // Machine-generated spec cannot promise an OAuth flow; this
                    // matches how automation presents credentials today.
                    "description":
                        "Personal access token issued under /api/auth/my/tokens.",
                },
            },
        },
        // BTreeMap serialises keys sorted, keeping output stable across runs.
        "paths": paths,
    })
}

/// Build one OpenAPI Operation object from a [`RouteSpec`] + HTTP verb.
fn operation(spec: &RouteSpec, method: &str) -> serde_json::Value {
    let tag = spec.tag.map_or_else(|| autotag(spec.path), str::to_string);
    let summary = spec
        .summary
        .map_or_else(|| autosummary(spec.path), str::to_string);
    let security = security_for(spec.path);
    serde_json::json!({
        // Unique per verb - a multi-method route yields one id per method, so ids
        // never collide (SDK generators index operations by id).
        "operationId": format!("{}_{}", method.to_lowercase(), tail_word(spec.path)),
        // Group endpoints into client-facing feature areas for SDK/client UIs.
        "tags": [tag],
        // Human-readable one-liner; every operation ships with meaning now
        // instead of an empty description (CXA-B047 / CXA-B051).
        "summary": summary,
        // Parameters come from :path segments; bodies are not modelled here.
        "parameters": parameters(spec.path),
        // Security tier mirrors auth_mw's real enforcement (see security_for).
        "security": security.requirement(),
        // Synthetic response contract - request/response bodies are not modelled,
        // so only an informative description ships.
        "responses": { "200": { "description": summary } },
    })
}

/// Security tier for a route derived from what auth_mw actually enforces in
/// [`super::auth`], so documentation cannot drift from runtime behaviour:
///
/// * **Public** - pass-through with no session (health, login, OpenAPI itself).
/// * **Admin** - management surfaces limited to Admin+ lead roles (`auth_mw`
///   treats `/api/auth/users*` and `/api/auth/tokens*` as manage-only).
/// * **Authenticated** - everything else needs any valid session plus membership
///   of any project named in the path.
fn security_for(path: &str) -> SecurityLevel {
    if matches!(
        path,
        "/api/health" | "/api/openapi.json" | "/api/auth/login"
    ) || path.starts_with("/s/")
    {
        // /s/:token (CXA-F069): the unguessable share token IS the credential,
        // so the page is public exactly like the login route.
        return SecurityLevel::Public;
    }
    if path.starts_with("/api/auth/users")
        || path.starts_with("/api/auth/tokens")
        || path.contains("/share-links")
    {
        // Share-link management is an admin surface: the handlers enforce
        // manage rights on top of auth_mw's membership gate (CXA-F069).
        return SecurityLevel::Admin;
    }
    SecurityLevel::Authenticated
}

#[cfg(test)]
fn tagged(path: &'static str) -> RouteSpec {
    RouteSpec {
        path,
        methods: &["get"],
        tag: None,
        summary: None,
    }
}

/// Feature-area tag derived from the first static segment after `/api/`. Stable
/// across routes sharing that prefix so clients see coherent groups rather than
/// one operation per resource; e.g. every `/api/projects/*` endpoint tags Projects.
fn autotag(path: &str) -> String {
    let rest = path.strip_prefix("/api/").unwrap_or("");
    rest.split('/')
        .next()
        .map(|seg| seg.replace(['-', '_'], ""))
        .filter(|s| !s.is_empty())
        .map_or_else(
            || String::from("Misc"),
            |s| {
                let mut chars = s.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            },
        )
}

/// One-line human summary derived from the last static segment of the path,
/// capitalised: reads like a terse instruction ("Tickets", "Metrics"). It gives
/// every operation a meaningful description without hand-authoring 159 strings;
/// an entry may override via [`RouteSpec::summary`] when path alone cannot
/// express intent.
fn autosummary(path: &str) -> String {
    let tail = tail_word(path);
    let last = tail.rsplit('_').next().unwrap_or("");
    if last.is_empty() {
        return String::from("Operation");
    }
    let mut chars = last.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::from("Operation"),
    }
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

    #[test]
    fn every_operation_carries_non_empty_tag_summary_and_security() {
        // CXA-C005 enrichment: no operation may ship an empty description or a
        // missing security requirement - SDK generators rely on all three.
        let doc = build_document();
        for item in doc["paths"].as_object().expect("paths object").values() {
            for op in item.as_object().expect("path item").values() {
                let tag = &op["tags"];
                assert!(
                    tag.as_array().is_some_and(|tags| !tags.is_empty()),
                    "operation lacks a tag: {:?}",
                    op["operationId"]
                );
                let summary = &op["summary"];
                assert!(
                    summary.as_str().is_some_and(|s| !s.is_empty()),
                    "operation lacks a summary: {:?}",
                    op["operationId"]
                );
                assert!(
                    op.get("security").is_some(),
                    "operation lacks security requirement: {:?}",
                    op["operationId"]
                );
            }
        }
    }

    #[test]
    fn tags_are_derived_from_the_first_api_segment() {
        // A project-scoped route groups under its leading feature area so SDKs
        // show coherent collections rather than one group per resource.
        let spec = tagged("/api/projects/:pid/metrics/summary");
        let op = operation(&spec, "get");
        assert_eq!(op["tags"], json!(["Projects"]));
    }
}
