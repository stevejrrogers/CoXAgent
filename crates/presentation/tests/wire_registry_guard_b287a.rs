//! CXA-B323 (re-lands CXA-B287a) — the wire-surface registry COUNT GUARD.
//!
//! The registry (`crates/presentation/src/server/wire_registry.rs`) claims to
//! enumerate every serde-derived request/response DTO on the hub's HTTP wire.
//! This is the test-time half of that promise: recount the surface directly
//! from the adapter sources and fail loudly when a row is missing, stale, or
//! duplicated. Part 3/3 contract-guard work builds on this scaffold.

// Guard tests assert by panicking; expect/unwrap ARE the failure path here.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use coxagent_presentation::server::wire_registry::{all_dto_names, WIRE_REGISTRY};

/// Derives-serde struct declarations found by scanning `src/server/*.rs`,
/// excluding in-file unit-test modules.
fn declared_serde_dtos() -> Vec<(String, String)> {
    // A missing src/server directory in a guard test IS the failure, so
    // expect/unwrap here are the assertion — lint exempted at fn level.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("list src/server") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.ends_with("_tests.rs") || name == "store_rpc_test_support.rs" {
            continue; // in-file fixtures, not the wire surface
        }
        let text = std::fs::read_to_string(&path).expect("read server source");
        // Strip #[cfg(test)] mod blocks so locally-derived fixture structs in
        // production files don't pollute the count.
        let text = strip_cfg_test_modules(&text);
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let derived = line.contains("#[derive(")
                && (line.contains("serde :: Deserialize")
                    || line.contains("serde::Deserialize")
                    || line.contains("serde :: Serialize")
                    || line.contains("serde::Serialize"));
            if derived {
                // Skip doc comments / blank lines between derive and decl.
                let mut j = i + 1;
                while j < lines.len()
                    && (lines[j].trim_start().starts_with("#[")
                        || lines[j].trim_start().starts_with("//")
                        || lines[j].trim().is_empty())
                {
                    j += 1;
                }
                if let Some(rest) = lines.get(j) {
                    let trimmed = rest.trim_start();
                    let trimmed = trimmed
                        .strip_prefix("pub(super) ")
                        .or_else(|| trimmed.strip_prefix("pub(crate) "))
                        .or_else(|| trimmed.strip_prefix("pub "))
                        .unwrap_or(trimmed);
                    if let Some(name) = trimmed.strip_prefix("struct ") {
                        if let Some(id) = name
                            .split(|c: char| !c.is_alphanumeric() && c != '_')
                            .next()
                        {
                            if !id.is_empty() {
                                out.push((id.to_string(), path_file_stem(&path)));
                                i = j;
                            }
                        }
                    }
                }
            }
            i += 1;
        }
    }
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0); // Beat lives inside store_rpc.rs once
    out
}

/// Remove trailing `#[cfg(test)] mod ... { ... }` blocks (brace-balanced).
fn strip_cfg_test_modules(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut lines = src.lines().peekable();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        if t.starts_with("#[cfg(test)]") {
            // hold the attribute; peek the next meaningful line
            let attr = line.to_string();
            let next = lines.peek().map_or("", |l: &&str| l.trim_start());
            if next.starts_with("mod ") {
                let mut depth = 0usize;
                for l in lines.by_ref() {
                    depth += l.matches('{').count();
                    depth -= l.matches('}').count();
                    if depth == 0 {
                        break;
                    }
                }
                continue; // dropped whole cfg(test) module incl. attr
            }
            out.push_str(&attr);
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn path_file_stem(path: &std::path::Path) -> String {
    path.file_stem().unwrap().to_string_lossy().to_string() + ".rs"
}

#[test]
fn registry_row_count_equals_the_declared_wire_surface() {
    let actual = declared_serde_dtos();
    assert_eq!(
        WIRE_REGISTRY.len(),
        actual.len(),
        "WIRE_REGISTRY declares {} rows but src/server defines {} serde-derived \
         DTO structs. Add/remove registry rows to match:\n{}",
        WIRE_REGISTRY.len(),
        actual.len(),
        diff_hint(&actual)
    );
}

#[test]
fn registry_rows_match_the_declared_surface_exactly() {
    let actual = declared_serde_dtos();
    for w in WIRE_REGISTRY {
        assert!(
            actual.iter().any(|(name, _file)| w.dto == name.as_str()),
            "registry row `{}` has no matching serde-derived struct under \
             src/server — remove it or restore the struct",
            w.dto
        );
    }
}

#[test]
fn every_owner_file_exists_under_src_server() {
    for w in WIRE_REGISTRY {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/server")
            .join(w.owner_file);
        assert!(
            path.exists(),
            "{} names missing file {}",
            w.dto,
            w.owner_file
        );
    }
}

#[test]
fn registry_has_no_duplicate_rows() {
    let names = all_dto_names();
    let unique: std::collections::BTreeSet<_> = names.iter().collect();
    assert_eq!(
        names.len(),
        unique.len(),
        "duplicate registry rows: a DTO may appear exactly once"
    );
}

fn diff_hint(actual: &[(String, String)]) -> String {
    let registry: std::collections::BTreeSet<&str> = WIRE_REGISTRY.iter().map(|w| w.dto).collect();
    let mut missing: Vec<String> = actual
        .iter()
        .filter(|(n, _)| !registry.contains(n.as_str()))
        .map(|(n, f)| format!("  + {n} ({f})"))
        .collect();
    let actual_names: std::collections::BTreeSet<&str> =
        actual.iter().map(|(n, _)| n.as_str()).collect();
    missing.extend(
        registry
            .difference(&actual_names)
            .map(|n| format!("  - {n}")),
    );
    missing.join("\n")
}
