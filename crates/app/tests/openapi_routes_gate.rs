//! CXA-F023 drift guard: every service route registered live in
//! `crates/presentation/src/server/mod.rs` must appear as an OpenAPI Path Item
//! served by `/api/openapi.json`.
//!
//! The hub ships no axum route introspection we can query at runtime cheaply, so
//! the spec mirrors registration explicitly and this guard pins them equal. It
//! scans both source files textually over their path literals — the same way other
//! gates here stay honest when there is no live surface to probe cheaply
//! (`docs_ports`, per-project gates).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Every quoted literal that immediately follows a given call prefix.
fn quoted_args(src: &str, prefix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(start) = rest.find(prefix) {
        rest = &rest[start + prefix.len()..];
        if !rest.starts_with('"') {
            continue;
        }
        rest = &rest[1..];
        let arg: String = rest.chars().take_while(|c| *c != '"').collect();
        if !arg.is_empty() {
            out.insert(arg);
        }
    }
    out
}

/// UI-shell / embedded-asset leaves deliberately not documented as service routes.
fn is_service_path(path: &str) -> bool {
    path != "/" && !path.starts_with("/assets/") && !path.starts_with("/join/")
}

#[test]
fn every_registered_service_route_is_documented_in_openapi() {
    let mod_src = read("crates/presentation/src/server/mod.rs");
    let spec_src = read("crates/presentation/src/server/openapi.rs");

    // Registered via axum's `.route(` builder; documented via the bare helper's
    // `route("` calls (the definition line carries no quoted literal).
    let registered: BTreeSet<String> = quoted_args(&mod_src, ".route(\"")
        .into_iter()
        .filter(|p| is_service_path(p))
        .collect();
    let documented: BTreeSet<String> = quoted_args(&spec_src, "route(\"")
        .into_iter()
        .filter(|p| is_service_path(p))
        .collect();

    // Both directions matter: a live route with no spec entry drifts silently,
    // but so does an orphan entry claiming a route that no longer exists.
    let undocumented: Vec<_> = registered.difference(&documented).collect();
    assert!(
        undocumented.is_empty(),
        "live routes not described by /api/openapi.json: {undocumented:?} \
         — add each to ROUTES in crates/presentation/src/server/openapi.rs"
    );
    let orphaned: Vec<_> = documented.difference(&registered).collect();
    assert!(
        orphaned.is_empty(),
        "OpenAPI documents routes serve_full() never registers (orphans): {orphaned:?} \
         — remove them from ROUTES or register them live"
    );
}
