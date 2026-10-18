//! CXA-B124 regression guard: an e2e acceptance gate must stay ARMED.
//!
//! The bug (twice on the same spec): `e2e/specs/deps-scan.spec.ts` was parked
//! in `test.fixme` on the claim that `POST /deps/scan` still hangs. Playwright
//! skips fixme tests, so the acceptance gate ran zero assertions while CI
//! stayed green — the route could regress silently. The hang claim turned out
//! false (CXA-B124), but the claim is not what this gate kills: the MECHANISM
//! is. A parked, skipped, only-filtered or commented-out acceptance test is a
//! blind gate; a deleted or renamed test title is a shrunken gate. Both fail
//! here, the same way `ci_availability_gate.rs` fails a re-gated CI job.
//!
//! Line comments are stripped before scanning, so prose in a spec header
//! cannot read as a parked test. Block comments are deliberately NOT
//! stripped: commenting out a test body is parking it — exactly the bug
//! class. A `//` inside a string literal would truncate that line; that can
//! only hide a marker, never invent one, and no spec string carries one.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

/// Where the product's acceptance gates live. Helpers (`*.mjs`) are not
/// gates and are not scanned.
const SPECS_DIR: &str = "e2e/specs";

/// The dependency-health acceptance gate whose parking motivated this file.
const DEPS_SCAN_SPEC: &str = "e2e/specs/deps-scan.spec.ts";

/// The CXA-B111 acceptance contract for the dependency scan, pinned so the
/// gate cannot shrink. Renaming or deleting one of these tests must fail
/// here and be answered in the same PR — lockstep, like gate_promises.rs.
const DEPS_SCAN_TITLES: &[&str] = &[
    "the dependency scan endpoint discovers real lockfiles and files a remediation ticket",
    "an empty snapshot is still a valid inventory-only scan",
];

/// Every way to park a Playwright test. `test.only` belongs here too: one
/// `.only` anywhere filters the whole run, silently skipping every other
/// acceptance gate in the suite.
const PARKED: &[&str] = &[
    "test.fixme",
    "test.skip",
    "test.todo",
    "test.only",
    "describe.fixme",
    "describe.skip",
    "describe.only",
];

/// The code-only view: drop `//` line comments so prose in a spec header
/// cannot read as a parked test.
fn code_only(src: &str) -> String {
    src.lines()
        .map(|l| l.split("//").next().unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Why is an acceptance gate blind? `Ok` only when no parking marker survives
/// comment stripping. Pure so the tests below can feed it synthetic specs.
fn why_not_armed(src: &str) -> Result<(), String> {
    let code = code_only(src);
    match PARKED.iter().find(|m| code.contains(*m)) {
        Some(m) => Err(format!(
            "`{m}` parks a test — a skipped acceptance gate is a blind gate. Fix the \
             behaviour and keep the test running, or argue the removal openly; never \
             silence a gate on an unverified claim."
        )),
        None => Ok(()),
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every acceptance spec on disk with its repo-relative path, sorted for a
/// deterministic failure message.
fn spec_sources() -> Vec<(String, String)> {
    let dir = repo_root().join(SPECS_DIR);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        let is_spec = p.extension().is_some_and(|x| x == "ts")
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".spec.ts"));
        if !is_spec {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&p) {
            let rel = format!("{SPECS_DIR}/{}", p.file_name().unwrap().to_string_lossy());
            out.push((rel, text));
        }
    }
    out.sort();
    out
}

#[test]
fn every_e2e_acceptance_gate_is_armed() {
    let specs = spec_sources();
    assert!(
        !specs.is_empty(),
        "no e2e specs found under {SPECS_DIR} — the acceptance suite itself is missing"
    );
    for (rel, text) in specs {
        if let Err(why) = why_not_armed(&text) {
            panic!("{rel}: {why}");
        }
    }
}

#[test]
fn the_deps_scan_gate_cannot_shrink() {
    let src = std::fs::read_to_string(repo_root().join(DEPS_SCAN_SPEC))
        .unwrap_or_else(|e| panic!("{DEPS_SCAN_SPEC} is missing or unreadable: {e}"));
    for title in DEPS_SCAN_TITLES {
        assert!(
            src.contains(title),
            "{DEPS_SCAN_SPEC} no longer declares `{title}` — the acceptance gate shrank. \
             Restore it or extend this pin in the same PR."
        );
    }
}

#[test]
fn a_reparked_gate_is_caught() {
    // The exact CXA-B124 regression: the 4b882820 shape, a fixme in code.
    let why =
        why_not_armed("test.fixme('the dependency scan endpoint', async () => {});").unwrap_err();
    assert!(why.contains("test.fixme"), "unhelpful: {why}");
    assert!(why.contains("blind gate"), "unhelpful: {why}");
}

#[test]
fn skip_only_and_todo_are_caught() {
    for parked in [
        "test.skip(",
        "test.only(",
        "test.todo(",
        "describe.only(",
        "describe.skip(",
    ] {
        assert!(why_not_armed(parked).is_err(), "{parked} slipped through");
    }
}

#[test]
fn prose_in_a_header_comment_is_not_a_parked_test() {
    // The real spec's re-enable note mentions `test.fixme` in prose; stripping
    // line comments must keep that legal while the code stays armed.
    let src = "// passes; do not park it in test.fixme on an unverified claim.\ntest('it works', () => {});";
    why_not_armed(src).unwrap_or_else(|why| panic!("prose read as a parked test: {why}"));
}

#[test]
fn commenting_out_a_test_body_is_caught() {
    // Block comments stay in the scan: a commented-out acceptance test is a
    // parked one, just with quieter handwriting.
    let why = why_not_armed("/* test.fixme('silenced', () => {}); */").unwrap_err();
    assert!(why.contains("test.fixme"), "unhelpful: {why}");
}
