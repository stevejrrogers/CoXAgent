//! CXA-F233 acceptance gate — "Cross-project live agent activity river".
//!
//! Written before implementation (TDD) so the acceptance criteria are pinned as
//! executable invariants over repo state and the existing state/domain types;
//! compiles today and fails only for the missing behaviour. PURE: no fake HTTP
//! server, no host harness, no network port. The wiring invariants are pinned
//! the same way `openapi_routes_gate.rs` pins route registration (scans of the
//! source-of-truth files), and the data invariants run over the real types the
//! river must forward (`RunnerSnapshot`, `ProjectState`, `completed_cycles`).
//!
//! Red today, and why:
//!   * AC1 — no `/api/fleet/river` route is registered (`serve_full`) or
//!     documented (openapi ROUTES), and no dashboard script subscribes to it.
//!     The hub-level path (no `:pid` segment) IS the "ALL registered projects
//!     at once" pin: a per-project stream would be a different literal.
//!     The per-project payload reuses the single-project events shape
//!     (`runner` + `state`, realtime.rs); the runner half is pinned green
//!     below, the lite-state half is produced by the private
//!     `lite_state_value` (server/status.rs) and is only assertable
//!     end-to-end once the endpoint exists — the pure-layer follow-up is to
//!     extract that payload builder as a public pure function and extend this
//!     gate to it.
//!   * AC3 — no view asset marks 'human action needed' events; the flag's
//!     data source (`ProjectState.human_holds`, the inbox's `human_eyes`
//!     source and the design's pinned human-hold signal) exists and is
//!     pinned green below.
//!   * AC4 — the fleet stream must dedupe viewers per authenticated user
//!     exactly like the single-project events stream. The only per-user
//!     mechanism in the codebase is the shared username-keyed registry
//!     (`app.viewers`) accessed through `ViewerGuard` (realtime.rs events_ep):
//!     one user in two tabs is one map entry, so map length IS distinct
//!     users. The river handler must register its connection through the
//!     same guard — per-connection counting cannot dedupe, and a second
//!     registry would split the count the two streams report.
//!   * AC5 — the exact '(insufficient data)' rendering exists nowhere in the
//!     delivered view or server code today (only in doc comments), and the
//!     river threshold is TWO completed cycles (trends uses three) — pinned
//!     green over the public `completed_cycles` scorecard reader.
//!
//! Design gaps reported to SA (not fabricated, per house rules):
//!   * AC2 — the wire contract for "filtered by project id and by agent
//!     phase" is unspecified: the query-parameter names appear nowhere in the
//!     codebase, and the "friendly empty-state payload" has no shape to
//!     assert. Pinning either would invent design. ASK SA for the filter
//!     parameters and the empty-state payload shape; AC2 then gets its
//!     executable invariants here.
//!   * AC3 — the event flag's payload field name is unspecified ('human
//!     action needed' is the AC's phrase, not a codebase identifier); the
//!     gate pins the view marker by the AC's own words.
//!
//! AC → test map:
//! - AC1: [`ac1_fleet_river_is_a_registered_get_route`],
//!   [`ac1_the_dashboard_subscribes_to_the_fleet_river_stream`],
//!   [`the_runner_phase_the_river_emits_is_the_existing_snapshot_shape`]
//! - AC2: no executable invariant — design gap above.
//! - AC3: [`the_human_action_flag_source_is_persisted_project_state`],
//!   [`ac3_human_action_needed_events_are_marked_in_the_dashboard_view`]
//! - AC4: [`ac4_the_fleet_stream_dedupes_viewers_like_the_single_project_stream`]
//! - AC5: [`the_insufficient_data_threshold_is_two_completed_cycles`],
//!   [`ac5_the_view_renders_insufficient_data_below_two_completed_cycles`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::metrics_health::completed_cycles;
use coxagent_application::state::{CycleScore, ProjectState};
use coxagent_application::use_cases::RunnerHandle;

/// The endpoint the acceptance criteria name — hub-level (no `:pid` segment),
/// which is what makes it cover every registered project in one stream.
const RIVER_ROUTE: &str = "/api/fleet/river";

// --- repo-state scan helpers (the openapi_routes_gate.rs pattern) -----------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// `(file name, source)` for every file with `ext` in `dir_rel`, sorted for
/// deterministic scans.
fn sources(dir_rel: &str, ext: &str) -> Vec<(String, String)> {
    let dir = repo_root().join(dir_rel);
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension()?.to_str()? == ext {
                let name = p.file_name()?.to_str()?.to_owned();
                let src = std::fs::read_to_string(&p).ok()?;
                Some((name, src))
            } else {
                None
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn server_sources() -> Vec<(String, String)> {
    sources("crates/presentation/src/server", "rs")
}

/// The dashboard view code: classic scripts sharing one scope, served by the
/// hub (see AGENTS.md — any UI change must pass the e2e gate).
fn web_js_sources() -> Vec<(String, String)> {
    sources("crates/presentation/src/web/js", "js")
}

/// Every quoted path literal that follows `prefix` — the same registration
/// detector `openapi_routes_gate.rs` uses, so this gate pins registration by
/// the exact mechanism the mirror gate enforces.
fn quoted_args(src: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(start) = rest.find(prefix) {
        rest = &rest[start + prefix.len()..];
        if let Some(after_quote) = rest.trim_start().strip_prefix('"') {
            if let Some(end) = after_quote.find('"') {
                out.push(after_quote[..end].to_owned());
                rest = &after_quote[end + 1..];
                continue;
            }
        }
        if rest.is_empty() {
            break;
        }
        rest = &rest[1..];
    }
    out
}

/// The `&[...]` methods slice of the openapi ROUTES entry for `path`, so the
/// documented METHOD is pinned too (the AC says GET).
fn openapi_route_methods(src: &str, path: &str) -> Option<String> {
    let needle = format!("route(\"{path}\",");
    let start = src.find(needle.as_str())?;
    let rest = &src[start + needle.len()..];
    let methods_start = rest.find("&[")?;
    let after = &rest[methods_start + 2..];
    let end = after.find(']')?;
    Some(after[..end].to_owned())
}

/// Case-insensitive whole-token search: "river" must match `/api/fleet/river`
/// and a `river_ep` handler, but not "drivers".
fn mentions_token(src: &str, token: &str) -> bool {
    let hay = src.to_lowercase();
    let token = token.to_lowercase();
    let bytes = hay.as_bytes();
    let mut from = 0;
    while let Some(i) = hay[from..].find(token.as_str()) {
        let at = from + i;
        let before_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let end = at + token.len();
        let after_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        from = at + 1;
    }
    false
}

/// A server source that implements the river: mentions the fleet-river route's
/// own tokens (the route string lives in mod.rs; a handler file carries them
/// in its function names — the house `*_ep` convention).
fn is_river_source(src: &str) -> bool {
    mentions_token(src, "river") || mentions_token(src, "fleet")
}

// --- fixtures over the real state/domain types ------------------------------

/// `n` completed cycles on the persisted scorecard ledger — the same fixture
/// shape `agent_cycle_metrics_dashboard.rs` seeds, built through the real
/// serde shape of `CycleScore`.
fn state_with_cycles(n: usize) -> ProjectState {
    let mut s = ProjectState::default();
    for i in 0..n {
        let cs: CycleScore = serde_json::from_value(serde_json::json!({
            "cycle": (i as u64) + 1,
            "at": "2026-08-01T00:00:00Z",
            "runs": 1, "useful": 1, "cost_usd": 1.0,
            "shipped": 1, "incidents": 0, "errors": 0, "grade": "B"
        }))
        .expect("cycle score");
        s.cycle_scores.push(cs);
    }
    s
}

// --- AC1 --------------------------------------------------------------------

/// AC1: "A GET /api/fleet/river endpoint ..." — the route must be registered
/// live in `serve_full` and documented in the OpenAPI mirror as a GET
/// (`openapi_routes_gate.rs` keeps the two equal; the AC pins the method).
#[test]
fn ac1_fleet_river_is_a_registered_get_route() {
    let mod_rs = read("crates/presentation/src/server/mod.rs");
    let registered = quoted_args(&mod_rs, ".route(");
    assert!(
        registered.contains(&RIVER_ROUTE.to_owned()),
        "serve_full must register {RIVER_ROUTE}; registered routes: {registered:?}"
    );

    let openapi = read("crates/presentation/src/server/openapi.rs");
    let methods = openapi_route_methods(&openapi, RIVER_ROUTE).unwrap_or_else(|| {
        panic!("{RIVER_ROUTE} must be documented in the openapi ROUTES table")
    });
    assert!(
        methods.contains("\"get\""),
        "the river must answer GET, found [{methods}]"
    );
}

/// AC1: "...streams server-sent events..." — the dashboard view must consume
/// the river as a stream, and `EventSource` is this codebase's only SSE client
/// pattern (docs.js, shell.js agent log). A stream consumed any other way
/// would be a novel pattern; the gate pins the house pattern.
#[test]
fn ac1_the_dashboard_subscribes_to_the_fleet_river_stream() {
    let scripts = web_js_sources();
    let (_, src) = scripts
        .iter()
        .find(|(_, s)| s.contains(RIVER_ROUTE))
        .unwrap_or_else(|| {
            panic!("no dashboard script references {RIVER_ROUTE}; the dashboard view never renders the river")
        });
    assert!(
        src.contains("EventSource"),
        "the river must be consumed as an SSE stream (EventSource), like every other stream in this dashboard"
    );
}

/// AC1 (green guard): "emitting each project's current runner phase" — the
/// phase data the river forwards is the existing `RunnerSnapshot`; pin the
/// fields the dashboard's phase indicator reads, exactly as the single-project
/// events stream emits them today (`mode` + active agent). Guards the river
/// against forwarding a reduced or renamed phase shape.
#[test]
fn the_runner_phase_the_river_emits_is_the_existing_snapshot_shape() {
    let runner = RunnerHandle::new();
    runner.resume();
    runner.set_active("DEV-FEATURE", "CXA-F233");

    let v = serde_json::to_value(runner.snapshot()).expect("snapshot serializes");
    assert_eq!(v["mode"], "running", "the runner phase the river emits");
    assert_eq!(v["active_role"], "DEV-FEATURE", "the agent working now");
    assert_eq!(v["active_note"], "CXA-F233", "what the agent is on");
}

// --- AC3 --------------------------------------------------------------------

/// AC3 (green guard): events "carrying a 'human action needed' flag" must be
/// derived from data that exists today — `ProjectState.human_holds`, the
/// codebase's canonical "held for a person" map (the inbox's `human_eyes`
/// items come from it, and the F233 design pins it as the human-hold signal).
/// The flag has a real persisted source; it must not be fabricated client-side.
#[test]
fn the_human_action_flag_source_is_persisted_project_state() {
    let mut s = ProjectState::default();
    assert!(
        s.human_holds.is_empty(),
        "a fresh project needs no human action"
    );
    s.human_holds.insert(7, "needs human eyes".to_owned());

    let v = serde_json::to_value(&s).expect("state serializes");
    assert_eq!(
        v["human_holds"]["7"],
        "needs human eyes",
        "the human-hold data the flag must be derived from rides in the project state the river forwards"
    );
}

/// AC3: "Events carrying a 'human action needed' flag are visually
/// distinguishable from routine progress events in the dashboard view." The
/// script that renders the river must mark such events — pinned by the AC's
/// own phrase, since no codebase identifier exists for the flag yet (see the
/// design-gaps note in the header).
#[test]
fn ac3_human_action_needed_events_are_marked_in_the_dashboard_view() {
    let scripts = web_js_sources();
    let (_, src) = scripts
        .iter()
        .find(|(_, s)| s.contains(RIVER_ROUTE))
        .unwrap_or_else(|| {
            panic!("no dashboard script references {RIVER_ROUTE}; the dashboard view never renders the river")
        });
    assert!(
        src.to_lowercase().contains("human action needed"),
        "the river view must visually distinguish 'human action needed' events from routine progress events (a visible marker/label naming the flag)"
    );
}

// --- AC4 --------------------------------------------------------------------

/// AC4: "Viewer counts for online users are deduplicated per authenticated
/// user (not per browser tab), matching the existing single-project
/// behavior." The single-project mechanism is the shared username-keyed
/// viewer registry (`app.viewers`) through `ViewerGuard` (realtime.rs
/// events_ep). The fleet stream must register its connection through the same
/// guard: per-connection counting cannot dedupe tabs, and a second registry
/// would split the count the two streams report for the same people.
#[test]
fn ac4_the_fleet_stream_dedupes_viewers_like_the_single_project_stream() {
    let sources = server_sources();
    assert!(
        sources.iter().any(|(_, src)| is_river_source(src)),
        "no server source implements the fleet river yet"
    );
    assert!(
        sources
            .iter()
            .any(|(_, src)| is_river_source(src) && src.contains("ViewerGuard::new(")),
        "the fleet river must count viewers through the shared per-user ViewerGuard registry (app.viewers), matching the single-project events stream"
    );
}

// --- AC5 --------------------------------------------------------------------

/// AC5 (green guard): "fewer than two cycles have completed" — the river must
/// read completed cycles from the persisted scorecard ledger via the existing
/// `completed_cycles` reader, not invent a counter. The threshold here is
/// deliberately TWO (the /metrics/trends AC5 uses three); the AC pins the
/// river's own, stricter edge.
#[test]
fn the_insufficient_data_threshold_is_two_completed_cycles() {
    let one = state_with_cycles(1);
    let two = state_with_cycles(2);

    assert_eq!(completed_cycles(&one), 1);
    assert!(
        completed_cycles(&one) < 2,
        "one completed cycle is insufficient data for the river"
    );
    assert!(
        completed_cycles(&two) >= 2,
        "two completed cycles are enough for the river"
    );
}

/// AC5: "The view renders '(insufficient data)' gracefully when fewer than
/// two cycles have completed for any included project." The exact string
/// exists nowhere in the delivered view or server code today (only in doc
/// comments), so the gate is red until the river view renders it — from the
/// view itself or from a payload the river server builds.
#[test]
fn ac5_the_view_renders_insufficient_data_below_two_completed_cycles() {
    let scripts = web_js_sources();
    let in_view = scripts.iter().any(|(_, src)| {
        src.contains(RIVER_ROUTE) && src.to_lowercase().contains("insufficient data")
    });

    let sources = server_sources();
    let from_server = sources.iter().any(|(_, src)| {
        is_river_source(src) && src.to_lowercase().contains("insufficient data")
    });

    assert!(
        in_view || from_server,
        "the river view must render '(insufficient data)' for any included project with fewer than two completed cycles"
    );
}
