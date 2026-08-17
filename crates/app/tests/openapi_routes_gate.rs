//! CXA-B051 drift guard: every service route registered live in
//! `crates/presentation/src/server/mod.rs` must appear as an OpenAPI Path Item
//! served by `/api/openapi.json`.
//!
//! The hub ships no axum route introspection we can query at runtime cheaply, so
//! the spec mirrors registration explicitly and this guard pins them equal. It
//! scans both source files textually over their path literals - the same way
//! other gates here stay honest when there is no live surface to probe cheaply.
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

/// Every quoted path literal that follows `prefix`, allowing whitespace/newlines
/// between the prefix and the opening quote (axum route chains often put the path
/// on its own line when a handler is long). `prefix` must not include the quote.
fn quoted_args(src: &str, prefix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(start) = rest.find(prefix) {
        rest = &rest[start + prefix.len()..];
        if let Some(after_quote) = rest.trim_start().strip_prefix('"') {
            if let Some(end) = after_quote.find('"') {
                out.insert(after_quote[..end].to_owned());
                // Advance past this whole literal before scanning for more.
                rest = &after_quote[end + 1..];
                continue;
            }
        }
        // Not a quoted arg here - step forward one char so we cannot spin on it.
        if rest.is_empty() {
            break;
        }
        rest = &rest[1..];
    }
    out
}

/// UI-shell / embedded-asset leaves deliberately not documented as service routes.
fn is_service_path(path: &str) -> bool {
    path != "/" && !path.starts_with("/assets/") && !path.starts_with("/join/")
}

#[test]
fn every_registered_service_route_is_documented_in_openapi() {
    // Registered via axum's `.route(` builder; documented via ROUTES entries,
    // which are indented (`\n   route(`) so they do not collide with other calls.
    //
    // Both directions matter: a live route with no spec entry drifts silently,
    // but so does an orphan entry claiming a route that no longer exists.
    let registered: BTreeSet<String> =
        quoted_args(&read("crates/presentation/src/server/mod.rs"), ".route(")
            .into_iter()
            .filter(|p| is_service_path(p))
            .collect();
    let documented: BTreeSet<String> =
        quoted_args(&read("crates/presentation/src/server/openapi.rs"), "\n    route(")
            .into_iter()
            .filter(|p| is_service_path(p))
            .collect();

    assert_eq!(
        registered,
        documented,
        "ROUTES in crates/presentation/src/server/openapi.rs must mirror exactly \
         what serve_full() registers; add missing routes or drop orphans"
    );
}
