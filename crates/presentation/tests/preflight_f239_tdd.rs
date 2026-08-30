//! CXA-F239 — Project go-live readiness preflight: is this workspace ready to
//! ship its first Verified ticket? RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "GET /api/projects/:pid/preflight returns one JSON object with named
//!    line items covering engines/models-per-role against config allowlist,
//!    host_port assignment & collision state, auth mode (open vs provisioned
//!    admin/token), docker+compose availability for deployability, and
//!    publish-port availability."
//! 2. "A project whose coxagent.json fails to parse is reported as BLOCKED on
//!    its config line naming the offending field, with NO default-guessing
//!    that would silently disable policy gates."
//! 3. "An environment with no account configured reports the 'running open'
//!    security posture as an explicit warning item in preflight output."
//! 4. "A model selected for any role that is absent from (or forbidden by)
//!    the engine/model allowlist flags that role+model as blocked, naming
//!    both."
//! 5. "Dashboard surfaces preflight with per-item status (ok/warn/blocked)
//!    and only renders the primary Resume/Step affordance when every
//!    hard-gate item is ok."
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the bytes the hub
//! actually serves — the router source (`server/mod.rs`), the OpenAPI table
//! (`server/openapi.rs`) and the run-control script (`web/js/mcp.js`) — the
//! same no-harness discipline as `dashboard_file_pickers.rs`. No server, no
//! port, no invented types: every identifier below exists in production
//! today, so the suite compiles and each failing assertion fails only
//! because CXA-F239's behaviour is missing.
//!
//! - AC1 is pinned where its contract is machine-checkable without a
//!   harness: the route must be registered with GET on the hub router, and
//!   the OpenAPI table must document it with a summary naming the five
//!   line-item areas (the table's own documented purpose for summary
//!   overrides: "when path alone cannot express intent"). The runtime
//!   response shape — one JSON object, the exact line-item fields — is
//!   verified against the live app in the QA phase.
//! - AC5 is pinned on the served dashboard bytes: the run-control script
//!   must fetch the project preflight, render per-item ok/warn/blocked
//!   statuses, and consult the preflight verdict inside `renderRunner` —
//!   the function that drives `ctl-primary`/`ctl-step` (the primary
//!   Resume/Step affordance). If the affordance's driver moves to another
//!   served script, move this guard with it.
//!
//! NOT ENCODED HERE — reported, not fabricated: the per-item SEMANTICS of
//! AC2/AC3/AC4 (a config line reported BLOCKED naming the offending field; a
//! 'running open' warning item; a role+model flagged blocked naming both)
//! assert response data whose type exists nowhere in the codebase. Pinning
//! them would require inventing the response contract (forbidden: no
//! invented identifiers) or a suite that fails to compile (forbidden: it
//! would poison `cargo test` for every other ticket). The INPUT facts all
//! exist and are already regression-pinned where they live — `parse_config`
//! fail-closed naming the field (config_parse.rs tests, COX-B043/B050),
//! `model_allowed` over `EngineMapping::resolve` (policy.rs tests), the hub's
//! `HubExtras.auth: Option` (None = "running open", builders.rs) — what does
//! not exist is CXA-F239's own deliverable: the aggregation that turns those
//! facts into named status items. That is the gap, and it is reported as
//! one rather than papered over with a fabricated response type.
//!
//! DESIGN DISCREPANCY to resolve before implementation: the committed SA
//! design for this ticket (.coxagent/design/CXA-F239_DESIGN.json) describes
//! a DIFFERENT surface — a CLI plus `GET /api/projects/:pid/go-live`
//! measuring clean/tests/lints/cross-compile/deploy-health — while the
//! ticket's acceptance criteria (encoded here) specify
//! `GET /api/projects/:pid/preflight` measuring engines/models, host_port,
//! auth mode, docker+compose and publish-port. One of the two is wrong;
//! this suite follows the acceptance criteria, which are the contract.

/// The hub's router source — where every `/api/...` route is registered.
const ROUTER: &str = include_str!("../src/server/mod.rs");

/// The OpenAPI table mirroring the router (the CXA-B051 drift guard lives on
/// it, so a registered route without a table entry fails CI anyway).
const OPENAPI: &str = include_str!("../src/server/openapi.rs");

/// The run-control script the dashboard serves: `renderRunner` drives the
/// runpill's primary Resume/Step affordance.
const RUN_CONTROL: &str = include_str!("../src/web/js/mcp.js");

/// The dashboard shell the router serves verbatim, carrying the runpill's
/// `ctl-primary` / `ctl-step` buttons.
const DASHBOARD: &str = include_str!("../src/web/index.html");

/// The five line-item areas AC1 names, as the codebase spells them: the
/// config allowlist, the per-project `deploy.host_port`, the auth posture,
/// the docker compose toolchain, and the publishable port.
const LINE_ITEM_AREAS: [&str; 6] = [
    "allowlist",
    "host_port",
    "auth",
    "docker",
    "compose",
    "publish",
];

/// Every `.route(` registration as `(path, method-router text)`. Whitespace
/// and newlines between the call, the path literal and the method routers are
/// allowed, matching how the router chain is formatted.
fn route_registrations(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(at) = rest.find(".route(") {
        let after = &rest[at + ".route(".len()..];
        let Some(quoted) = after.trim_start().strip_prefix('"') else {
            rest = after;
            continue;
        };
        let Some(path_end) = quoted.find('"') else {
            rest = after;
            continue;
        };
        let path = quoted[..path_end].to_owned();
        // The method routers sit between the path and the registration's
        // closing paren; cap the scan at the next registration so one odd
        // literal cannot swallow the rest of the file.
        let tail = &quoted[path_end + 1..];
        let close = tail.find(')').map_or(tail.len(), |e| e);
        let next = tail.find(".route(").map_or(tail.len(), |e| e);
        let end = close.min(next);
        out.push((path, tail[..end].to_owned()));
        rest = &tail[end..];
    }
    out
}

/// The HTTP method the registration answers first (axum chains the rest):
/// the identifier at the head of the method-router text.
fn first_method(body: &str) -> &str {
    let b = body.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    let end = b
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map_or(b.len(), |e| e);
    &b[..end]
}

/// The OpenAPI table entry that mentions `path`: from the `route(` opener
/// before it to the start of the next entry. `None` when the table does not
/// document the path at all.
fn openapi_entry(src: &str, path: &str) -> Option<String> {
    let needle = format!("\"{path}\"");
    let at = src.find(&needle)?;
    let start = src[..at].rfind("route(")?;
    let end = src[at..]
        .find("\n    route(")
        .map_or(src.len(), |rel| at + rel);
    Some(src[start..end].to_owned())
}

/// One top-level `function name(…)` from a dashboard script, split on
/// `function ` headers the way `health_gate.rs` splits Rust fns: braces
/// inside string literals do not nest with the code's, so no brace matching.
fn js_fn(src: &str, name: &str) -> Option<String> {
    let header = format!("function {name}(");
    let at = src.find(&header)?;
    let rest = &src[at..];
    let end = rest[header.len()..]
        .find("\nfunction ")
        .map_or(rest.len(), |rel| header.len() + rel);
    Some(rest[..end].to_owned())
}

/// The window of source starting at the line that first names `marker` and
/// running `lines` further lines — where a preflight renderer would put its
/// status mapping. Empty when the marker is absent, which is its own
/// (failing) answer.
fn window_after<'a>(src: &'a str, marker: &str, lines: usize) -> &'a str {
    let Some(at) = src.find(marker) else {
        return "";
    };
    let line_start = src[..at].rfind('\n').map_or(at, |n| n + 1);
    let mut end = line_start;
    for _ in 0..lines {
        if let Some(n) = src[end..].find('\n') {
            end += n + 1;
        } else {
            end = src.len();
            break;
        }
    }
    &src[line_start..end]
}

// ---------------------------------------------------------------------------
// Fixture sanity — the guards stand on bytes the hub really serves.
// ---------------------------------------------------------------------------

#[test]
fn the_guards_stand_on_surfaces_the_hub_actually_serves() {
    assert!(
        ROUTER.contains("include_str!(\"../web/index.html\")"),
        "the router no longer embeds src/web/index.html — these guards assert \
         on markup nobody serves; point them at the file that is"
    );
    assert!(
        DASHBOARD.contains("mcp.js"),
        "the dashboard shell no longer loads the run-control script — the AC5 \
         guards below must follow it to whatever script drives the runpill"
    );
    assert!(
        DASHBOARD.contains("ctl-primary") && DASHBOARD.contains("ctl-step"),
        "the runpill's primary/step affordances are gone from the shell — \
         AC5 has nothing left to gate"
    );
    assert!(
        js_fn(RUN_CONTROL, "renderRunner").is_some(),
        "renderRunner vanished from the run-control script — it is what drives \
         ctl-primary/ctl-step, so the AC5 gating guard must follow it"
    );
}

// ---------------------------------------------------------------------------
// AC1 — GET /api/projects/:pid/preflight, one object, five named line items
// ---------------------------------------------------------------------------

/// AC1, registration half: the preflight endpoint must exist on the hub
/// router, answering GET under the per-project scope.
#[test]
fn ac1_the_preflight_route_is_registered_with_get_on_the_hub_router() {
    let registrations = route_registrations(ROUTER);
    let preflight: Vec<&(String, String)> = registrations
        .iter()
        .filter(|(p, _)| p.as_str() == "/api/projects/:pid/preflight")
        .collect();
    assert!(
        !preflight.is_empty(),
        "GET /api/projects/:pid/preflight is not registered on the hub router \
         — the go-live readiness preflight (CXA-F239 AC1) does not exist yet"
    );
    assert!(
        preflight
            .iter()
            .any(|(_, body)| first_method(body) == "get"),
        "/api/projects/:pid/preflight is registered but does not answer GET; \
         the registrations found: {preflight:?}"
    );
}

/// AC1, contract half: the API's self-description must document the route
/// with GET and a summary naming the five line-item areas — engines/
/// models-per-role against the config allowlist, host_port assignment &
/// collision, auth mode, docker+compose availability, and publish-port
/// availability. The table overrides summaries exactly for paths whose
/// intent the path alone cannot express; `/preflight` is that case.
#[test]
fn ac1_openapi_documents_preflight_with_a_summary_naming_its_line_item_areas() {
    let Some(entry) = openapi_entry(OPENAPI, "/api/projects/:pid/preflight") else {
        panic!(
            "/api/projects/:pid/preflight is missing from the OpenAPI ROUTES \
             table (the CXA-B051 drift gate fails the registration anyway) — \
             CXA-F239 AC1's contract surface does not exist yet"
        );
    };
    assert!(
        entry.contains("\"get\""),
        "the preflight entry must document GET: {entry}"
    );
    let lowered = entry.to_ascii_lowercase();
    for area in LINE_ITEM_AREAS {
        assert!(
            lowered.contains(area),
            "the preflight summary must name the '{area}' line-item area — \
             the line items AC1 requires are not part of the documented \
             contract: {entry}"
        );
    }
}

// ---------------------------------------------------------------------------
// AC5 — dashboard per-item statuses; primary affordance gated on hard gates
// ---------------------------------------------------------------------------

/// AC5, per-item half: the run-control script must fetch the project
/// preflight and render its items with the three statuses the criteria name.
#[test]
fn ac5_the_dashboard_fetches_preflight_and_renders_per_item_status() {
    assert!(
        RUN_CONTROL.contains("\"/preflight\""),
        "the run-control script never fetches the project preflight \
         (api(\"/preflight\")) — the dashboard has no per-item statuses to \
         surface (CXA-F239 AC5)"
    );
    let near = window_after(RUN_CONTROL, "\"/preflight\"", 60).to_ascii_lowercase();
    for status in ["ok", "warn", "blocked"] {
        assert!(
            near.contains(status),
            "the preflight rendering must carry the '{status}' item status \
             (CXA-F239 AC5) — nothing in the preflight-handling code maps it"
        );
    }
}

/// AC5, gating half: `renderRunner` owns the primary Resume/Step affordance
/// (`ctl-primary`, `ctl-step`), so it must consult the preflight verdict
/// there — otherwise nothing implements "only renders the primary Resume/Step
/// affordance when every hard-gate item is ok". That every hard-gate item is
/// indeed required to be ok (warn/blocked both suppress the affordance) is
/// runtime behaviour, verified against the live app in the QA phase.
#[test]
fn ac5_render_runner_gates_the_primary_affordance_on_the_preflight_verdict() {
    let Some(runner) = js_fn(RUN_CONTROL, "renderRunner") else {
        panic!("renderRunner vanished from the run-control script — the \
                fixture sanity test should have caught this first");
    };
    assert!(
        runner.to_ascii_lowercase().contains("preflight"),
        "renderRunner drives ctl-primary/ctl-step without consulting the \
         preflight verdict — the primary Resume/Step affordance renders \
         regardless of hard-gate items (CXA-F239 AC5)"
    );
}
