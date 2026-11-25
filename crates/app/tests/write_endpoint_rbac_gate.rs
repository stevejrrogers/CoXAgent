//! Authorization tripwire: every mutating HTTP route (POST/PUT/PATCH/DELETE)
//! registered in `serve_full()` must be listed in [`MUTATING_ROUTES`] below.
//!
//! WHY: `auth_mw` gates writes through `write_gate_ok`
//! (`crates/presentation/src/server/auth.rs`), which is a DENY-LIST — a route
//! that matches no PR-review / store-RPC / service-management pattern falls
//! through to the permissive default `role.can_write()`. So a newly added
//! privileged endpoint is, by default, open to every write-capable role unless
//! the developer remembers to classify it. The type system never catches this,
//! and the whole RBAC model is only as strong as that human memory.
//!
//! This gate converts the deny-list into a MUST-DECLARE registry: the set of
//! live mutating routes must equal [`MUTATING_ROUTES`] exactly. Add a mutating
//! endpoint and CI fails here until you (1) confirm its authorization in
//! `write_gate_ok` — plain `can_write`, or `can_manage`/`can_review` via the
//! appropriate `is_*_path` pattern — and (2) record it below. Remove one and
//! the orphan entry fails too. The tripwire cannot assert the capability tier
//! across crates (`write_gate_ok` is `pub(super)`), but it guarantees no
//! mutating route is ever added without a conscious pass over the auth gate.
//!
//! Extraction mirrors `openapi_routes_gate`: pure text parsing of
//! `crates/presentation/src/server/mod.rs`, since the hub ships no cheap
//! runtime route introspection.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Extract the path literals of every `.route(` whose method-router body builds
/// at least one mutating method (`post`/`put`/`patch`/`delete`). Pure.
fn mutating_routes(src: &str) -> BTreeSet<String> {
    const NEEDLE: &str = ".route(";
    let bytes = src.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while let Some(rel) = src[i..].find(NEEDLE) {
        let start = i + rel + NEEDLE.len();
        // Walk to the matching close paren of this `.route( ... )` call.
        let mut depth = 1usize;
        let mut p = start;
        while p < bytes.len() && depth > 0 {
            match bytes[p] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            p += 1;
        }
        let body = &src[start..p.saturating_sub(1)];
        i = p;
        let Some(path) = first_string_literal(body) else { continue };
        if has_mutating_method(body) {
            out.insert(path);
        }
    }
    out
}

/// The first `"..."` literal in `body` (the route path), if any.
fn first_string_literal(body: &str) -> Option<String> {
    let after = body.find('"')? + 1;
    let rest = &body[after..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Does the route body build a mutating method? Matches a method name only when
/// immediately followed by `(` — the axum builder form `post(handler)` — so a
/// handler identifier like `post_comment` or a quoted path segment never
/// counts.
fn has_mutating_method(body: &str) -> bool {
    ["post(", "put(", "patch(", "delete("]
        .iter()
        .any(|m| body.contains(m))
}

#[test]
fn every_mutating_route_is_declared_in_the_rbac_registry() {
    let src = std::fs::read_to_string(repo_root().join("crates/presentation/src/server/mod.rs"))
        .expect("read server mod.rs");
    let live = mutating_routes(&src);
    let declared: BTreeSet<String> = MUTATING_ROUTES.iter().map(|s| (*s).to_owned()).collect();

    let undeclared: Vec<&String> = live.difference(&declared).collect();
    let orphaned: Vec<&String> = declared.difference(&live).collect();

    assert!(
        undeclared.is_empty() && orphaned.is_empty(),
        "write-endpoint RBAC registry drifted from serve_full().\n\
         UNDECLARED mutating routes (confirm authorization in write_gate_ok, then add here):\n{}\n\
         ORPHANED registry entries (route gone — remove here):\n{}",
        undeclared.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n"),
        orphaned.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n"),
    );
}

#[test]
fn extractor_detects_a_mutating_route_and_skips_a_read_only_one() {
    let sample = r#"
        .route("/api/thing", get(list_things).post(make_thing))
        .route("/api/readonly", get(just_read))
        .route("/api/wipe", delete(wipe))
    "#;
    let found = mutating_routes(sample);
    assert!(found.contains("/api/thing"), "post route must be detected");
    assert!(found.contains("/api/wipe"), "delete route must be detected");
    assert!(!found.contains("/api/readonly"), "get-only route must be skipped");
}

/// Every mutating route live in `serve_full()`. Keep sorted. See the module
/// doc: adding an entry is a declaration that its authorization in
/// `write_gate_ok` has been reviewed.
const MUTATING_ROUTES: &[&str] = &[
    "/api/analyze-goal",
    "/api/auth/2fa/disable",
    "/api/auth/2fa/enable",
    "/api/auth/2fa/enroll",
    "/api/auth/login",
    "/api/auth/logout",
    "/api/auth/my/tokens",
    "/api/auth/my/tokens/:label",
    "/api/auth/profile",
    "/api/auth/tokens",
    "/api/auth/tokens/:label",
    "/api/auth/users",
    "/api/auth/users/:username",
    "/api/auth/users/:username/password",
    "/api/chat/channel/:cid/topic",
    "/api/chat/channels",
    "/api/chat/channels/:cid",
    "/api/chat/channels/:cid/invite",
    "/api/chat/channels/:cid/members/:member",
    "/api/chat/channels/:cid/settings",
    "/api/chat/channels/:cid/topic",
    "/api/chat/dm",
    "/api/chat/hook/:token",
    "/api/chat/messages/:mid",
    "/api/chat/messages/:mid/pin",
    "/api/chat/messages/:mid/reply",
    "/api/chat/react",
    "/api/chat/send",
    "/api/chat/upload",
    "/api/chat/webhooks",
    "/api/chat/webhooks/:token",
    "/api/fleet/ceiling",
    "/api/mcp",
    "/api/me/llm-keys",
    "/api/me/llm-keys/:id",
    "/api/me/llm-keys/:id/retest",
    "/api/meetings",
    "/api/meetings/:id",
    "/api/meetings/:id/join",
    "/api/meetings/:id/ring",
    "/api/pr-report",
    "/api/profile",
    "/api/profile/avatar",
    "/api/projects",
    "/api/projects/:pid",
    "/api/projects/:pid/alerts/:id/replay",
    "/api/projects/:pid/approval-policy/ask-again",
    "/api/projects/:pid/approval-policy/release",
    "/api/projects/:pid/architecture-review",
    "/api/projects/:pid/ba-analyze",
    "/api/projects/:pid/brakes/:brake/hold",
    "/api/projects/:pid/burn-mode",
    "/api/projects/:pid/channels",
    "/api/projects/:pid/channels/:cid/invite",
    "/api/projects/:pid/channels/:cid/members/:member",
    "/api/projects/:pid/channels/:cid/settings",
    "/api/projects/:pid/chat",
    "/api/projects/:pid/chat-reply",
    "/api/projects/:pid/codegraph/build",
    "/api/projects/:pid/comments",
    "/api/projects/:pid/comments/:id/react",
    "/api/projects/:pid/config",
    "/api/projects/:pid/context",
    "/api/projects/:pid/control/:action",
    "/api/projects/:pid/deps/scan",
    "/api/projects/:pid/digest",
    "/api/projects/:pid/discuss",
    "/api/projects/:pid/doc-folders",
    "/api/projects/:pid/doc-folders/delete",
    "/api/projects/:pid/docs-review",
    "/api/projects/:pid/docs/:id",
    "/api/projects/:pid/docs/:id/ai-edit",
    "/api/projects/:pid/docs/:id/move",
    "/api/projects/:pid/docs/generate",
    "/api/projects/:pid/git/connect",
    "/api/projects/:pid/git/test",
    "/api/projects/:pid/goals",
    "/api/projects/:pid/goals/:gid/rename",
    "/api/projects/:pid/lessons/dismiss",
    "/api/projects/:pid/lessons/escalate",
    "/api/projects/:pid/members",
    "/api/projects/:pid/members/:username",
    "/api/projects/:pid/merge-sweep",
    "/api/projects/:pid/milestone-complete/:name",
    "/api/projects/:pid/operators/:operator/:action",
    "/api/projects/:pid/pr/:number/human",
    "/api/projects/:pid/prs/:num/:action",
    "/api/projects/:pid/reverts/:sha",
    "/api/projects/:pid/services",
    "/api/projects/:pid/services/:name/status",
    "/api/projects/:pid/services/assignments/:username",
    "/api/projects/:pid/services/retag",
    "/api/projects/:pid/services/scan",
    "/api/projects/:pid/share-links",
    "/api/projects/:pid/share-links/:token",
    "/api/projects/:pid/sprint-queue",
    "/api/projects/:pid/sprint-queue/:qid",
    "/api/projects/:pid/sprint-queue/:qid/move/:dir",
    "/api/projects/:pid/sprint-queue/:qid/rename",
    "/api/projects/:pid/sprint-queue/:qid/scope",
    "/api/projects/:pid/sprint/:action",
    "/api/projects/:pid/sprint/close",
    "/api/projects/:pid/sprint/goal",
    "/api/projects/:pid/standup",
    "/api/projects/:pid/store",
    "/api/projects/:pid/ticket-refine",
    "/api/projects/:pid/ticket/:id/approve-cost",
    "/api/projects/:pid/ticket/:id/assign",
    "/api/projects/:pid/ticket/:id/attachments",
    "/api/projects/:pid/ticket/:id/edit",
    "/api/projects/:pid/ticket/:id/goal",
    "/api/projects/:pid/ticket/:id/handback",
    "/api/projects/:pid/ticket/:id/priority",
    "/api/projects/:pid/ticket/:id/ready",
    "/api/projects/:pid/ticket/:id/reject",
    "/api/projects/:pid/ticket/:id/send-back",
    "/api/projects/:pid/ticket/:id/status/:action",
    "/api/projects/:pid/ticket/:id/takeover",
    "/api/projects/:pid/ticket/:id/undo-approval",
    "/api/projects/:pid/ticket/:id/unpark",
    "/api/projects/:pid/ticket/:id/verify",
    "/api/projects/:pid/tickets",
    "/api/projects/:pid/upload",
    "/api/spaces",
    "/api/spaces/:sid",
    "/api/workspace",
    "/api/workspace/duplicates/action",
    "/api/workspace/duplicates/scan",
    "/api/workspace/invites",
    "/api/workspace/invites/:token",
    "/api/workspace/join",
];
