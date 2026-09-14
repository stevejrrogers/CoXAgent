//! CXA-F249 acceptance gate — forward milestone-completion projection with
//! blocked-scope surfacing.
//!
//! The drift-gated half pins the endpoint the acceptance criteria name the
//! same way `openapi_routes_gate.rs` / `fleet_river_f233_gate.rs` pin theirs:
//! registered live in `serve_full` AND documented in the OpenAPI ROUTES table
//! with the contracted method, so CI fails if either side regresses. The
//! pure-layer half pins the read model the route serves over real state
//! types: no server, no harness, no network port.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use coxagent_application::milestone_projection::{project_milestones, projection_report};
use coxagent_application::state::ProjectState;

const ROUTE: &str = "/api/projects/:pid/milestones/projection";

// --- repo-state scan helpers (the openapi_routes_gate.rs pattern) -----------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn quoted_args(src: &str, prefix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(start) = rest.find(prefix) {
        rest = &rest[start + prefix.len()..];
        if let Some(after_quote) = rest.trim_start().strip_prefix('"') {
            if let Some(end) = after_quote.find('"') {
                out.insert(after_quote[..end].to_owned());
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

fn openapi_route_methods(src: &str, path: &str) -> Option<String> {
    let needle = format!("route(\"{path}\",");
    let start = src.find(needle.as_str())?;
    let rest = &src[start + needle.len()..];
    let methods_start = rest.find("&[")?;
    let after = &rest[methods_start + 2..];
    let end = after.find(']')?;
    Some(after[..end].to_owned())
}

// --- the endpoint is registered live and documented as a GET ----------------

#[test]
fn the_projection_is_a_registered_get_route_documented_in_openapi() {
    let mod_rs = read("crates/presentation/src/server/mod.rs");
    let registered = quoted_args(&mod_rs, ".route(");
    assert!(
        registered.contains(ROUTE),
        "serve_full must register {ROUTE}; the projection endpoint regressed"
    );

    let openapi = read("crates/presentation/src/server/openapi.rs");
    let methods = openapi_route_methods(&openapi, ROUTE)
        .unwrap_or_else(|| panic!("{ROUTE} must be documented in the openapi ROUTES table"));
    assert!(
        methods.contains("\"get\""),
        "the projection must answer GET, found [{methods}]"
    );
}

// --- the pure read model the route serves -----------------------------------

#[test]
fn an_empty_roadmap_projects_nothing_and_the_report_stays_contract_shaped() {
    let report = projection_report(&ProjectState::default());
    assert!(project_milestones(&ProjectState::default()).is_empty());
    assert!(report.blocked_tickets.is_empty());
    assert_eq!(report.current_version, "0.0.0");

    let v = serde_json::to_value(&report).expect("report serializes");
    let keys: BTreeSet<_> = v.as_object().expect("object").keys().cloned().collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "blocked_tickets".to_owned(),
            "current_version".to_owned(),
            "milestones".to_owned(),
        ]),
        "the wire is exactly the contract"
    );
}
