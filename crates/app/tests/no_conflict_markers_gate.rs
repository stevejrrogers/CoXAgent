//! Regression guard: no tracked source file may carry an unresolved git
//! merge-conflict marker.
//!
//! The bug: `deploy/autoheal-cxa.sh` — the Postgres/Redis backing-stack
//! autohealer — was committed at HEAD with the raw `<<<<<<<` / `=======` /
//! `>>>>>>>` markers from a `feat/CXA-B224` merge still in it. `bash` errors on
//! the first marker line, so the whole script was dead: the entire DB/Redis
//! outage class had no working automated remedy, silently, for as long as the
//! markers sat there. A conflict marker committed to a shell script or a Rust
//! source is never intentional and never caught by a type checker, so a
//! structural guard is the only thing that stops it recurring.
//!
//! The scan is pure over `(path, contents)` pairs and value-independent. It
//! does NOT flag a lone marker line: prose legitimately starts a line with
//! `>>>>>>> ` (e.g. `prompts.rs` explaining that diffs with committed
//! `>>>>>>>` markers are never merged). Instead it matches git's actual
//! three-part structure IN ORDER — an open (`<<<<<<< `), then a divider line
//! that is exactly seven `=`, then a close (`>>>>>>> `) — which only an
//! unresolved conflict produces. The needles are built at run time from
//! repeated characters so this test file's own source never contains a marker
//! at column zero and never trips itself.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

/// An unresolved conflict hunk, reported at the line of its `<<<<<<<` open.
#[derive(Debug, PartialEq, Eq)]
struct Marker {
    path: String,
    line: usize,
    kind: &'static str,
}

/// Conflict-marker prefixes/divider, assembled so no literal run of seven
/// marker characters ever appears in this source.
fn open_needle() -> String {
    format!("{} ", "<".repeat(7))
}
fn close_needle() -> String {
    format!("{} ", ">".repeat(7))
}
fn divider() -> String {
    "=".repeat(7)
}

/// Scan one file's text for a git conflict's three-part structure IN ORDER:
/// an open line, a later divider line (exactly seven `=`), then a close line.
/// A lone marker line (common in prose about conflicts) is not a conflict.
/// Pure.
fn scan(path: &str, contents: &str) -> Vec<Marker> {
    let open = open_needle();
    let close = close_needle();
    let div = divider();
    // state: 0 = want open, 1 = have open/want divider, 2 = have divider/want close
    let mut state = 0u8;
    let mut open_line = 0usize;
    let mut hits = Vec::new();
    for (i, line) in contents.lines().enumerate() {
        if line.starts_with(open.as_str()) {
            state = 1;
            open_line = i + 1;
        } else if state == 1 && line.trim_end() == div {
            state = 2;
        } else if state == 2 && line.starts_with(close.as_str()) {
            hits.push(Marker { path: path.to_string(), line: open_line, kind: "conflict" });
            state = 0;
        }
    }
    hits
}

/// Extensions worth scanning — source and config text, not binaries or locks.
fn is_text_source(path: &Path) -> bool {
    const EXTS: &[&str] = &[
        "rs", "sh", "toml", "yml", "yaml", "md", "js", "ts", "jsx", "tsx",
        "css", "html", "json", "py", "swift", "sql",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Directories never worth walking (build output, deps, vendored, VCS).
fn is_skippable_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | "dist"
            | "build"
            | ".coxagent-worktrees"
            | "test-results"
            | "playwright-report"
            | ".state"
    )
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !is_skippable_dir(&name) {
                walk(&path, out);
            }
        } else if is_text_source(&path) {
            out.push(path);
        }
    }
}

fn workspace_root() -> PathBuf {
    // crates/app/tests/<this> → repo root is three parents up from the crate.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .unwrap()
}

#[test]
fn no_tracked_source_carries_a_conflict_marker() {
    let root = workspace_root();
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(
        files.len() > 100,
        "walk found only {} files — the scan is not reaching the tree",
        files.len()
    );

    let mut markers = Vec::new();
    for file in &files {
        // Non-UTF8 or oversized blobs can't carry a meaningful text marker.
        let Ok(meta) = fs::metadata(file) else { continue };
        if meta.len() > 4_000_000 {
            continue;
        }
        let Ok(bytes) = fs::read(file) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        markers.extend(scan(&rel, &text));
    }

    assert!(
        markers.is_empty(),
        "unresolved git conflict markers found in tracked source:\n{}",
        markers
            .iter()
            .map(|m| format!("  {}:{} ({} marker)", m.path, m.line, m.kind))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn scan_catches_the_autoheal_shape() {
    // The exact pre-fix shape (markers built at run time), proving the guard
    // fails on the unresolved state it is meant to block. Mirrors git diff3:
    // open, base, divider, close.
    let open = format!("{} HEAD", "<".repeat(7));
    let base = format!("{} a3523607", "|".repeat(7));
    let div = "=".repeat(7);
    let close = format!("{} feat/CXA-B224-panel-shell-pilot", ">".repeat(7));
    let sample =
        format!("#!/usr/bin/env bash\n{open}\nset -u\n{base}\n{div}\nset -u\n{close}\n");
    let hits = scan("deploy/autoheal-cxa.sh", &sample);
    assert_eq!(hits.len(), 1, "expected one conflict hunk");
    assert_eq!(hits[0].kind, "conflict");
    assert_eq!(hits[0].line, 2, "reported at the open-marker line");
}

#[test]
fn scan_ignores_a_lone_close_marker_in_prose() {
    // prompts.rs legitimately wraps a sentence onto a line beginning with the
    // close marker; with no open+divider before it, that is not a conflict.
    let prose = format!(
        "Diffs with committed markers are never merged.\n{} markers are never merged.\n",
        ">".repeat(7)
    );
    assert!(scan("crates/application/src/prompts.rs", &prose).is_empty());
}

#[test]
fn scan_passes_clean_text() {
    let clean = "#!/usr/bin/env bash\nset -u\necho ok\n# a ===== divider is fine\n";
    assert!(scan("deploy/x.sh", clean).is_empty());
}
