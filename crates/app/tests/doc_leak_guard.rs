//! CXA-B315 regression guard: no credential-shaped string reappears in this
//! repo's documentation or non-code config.
//!
//! The bug class (CXA-B311 → CXA-B031, SEC-41E384): a doc that documents the
//! deploy-secrets machinery was written with a live connection URL in it; the
//! scrub removed it, but nothing stopped the next doc from repeating the
//! mistake. Docs and prose configs have no review lane of their own — code has
//! `security_sweep`, compose files have `compose_security_gate` — so this gate
//! is docs' missing one.
//!
//! Split per the house rules:
//!
//! - the detection decision is [`coxagent_application::doc_leak_scan::scan_doc`],
//!   a PURE function over `(path, contents) -> findings` with its own unit tests
//!   beside it — no IO reaches the logic;
//! - this file is the infrastructure adapter: it walks the tracked tree (via a
//!   real git, same discipline as [`submodule_gate`] against the PATH shim),
//!   feeds each scanned file to the pure detector, and renders the failure;
//! - the synthetic cases at the bottom prove the guard bites both ways BEFORE
//!   it is pointed at the real tree: a planted fake credential must produce an
//!   attributed finding (file + line + rule), and the sanitized B031-style doc
//!   form must scan clean.

#![allow(clippy::unwrap_used)]

use coxagent_application::doc_leak_scan::{is_scanned_path, scan_doc};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A `git ls-files -z` listing: path → present (contents read separately).
type TrackedFiles = Vec<String>;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Where a real `git` lives, in preference order — see [`submodule_gate`] for
/// why the PATH shim cannot be trusted with a full listing.
const GIT_CANDIDATES: &[&str] = &["/usr/bin/git", "/opt/homebrew/bin/git", "git"];

/// Paths the index must contain for the listing to be trusted. They span the
/// tree's areas, so a listing truncated by the PATH shim fails the sentinel
/// check instead of scanning clean over a fraction of the repo.
const SENTINELS: &[&str] = &[
    ".gitignore",
    "Cargo.toml",
    "docker-compose.yml",
    "README.md",
    "docs/wiki/engineering/deploy-secrets.md",
];

fn missing_sentinels(paths: &[String]) -> Vec<&'static str> {
    SENTINELS
        .iter()
        .filter(|s| !paths.iter().any(|p| p == *s))
        .copied()
        .collect()
}

/// Every tracked path, via a real git, or an error naming what was tried.
/// There is no way to answer "what does the repo track?" without asking git,
/// so an answer we cannot verify as complete fails rather than passing clean.
fn tracked_files() -> Result<TrackedFiles, String> {
    let root = repo_root();
    let mut tried = Vec::new();
    for git in GIT_CANDIDATES {
        let out = match Command::new(git)
            .arg("-C")
            .arg(&root)
            .args(["ls-files", "-z"])
            .output()
        {
            Ok(out) if out.status.success() => out,
            Ok(out) => {
                tried.push(format!(
                    "{git}: exited {} ({})",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
                continue;
            }
            Err(e) => {
                tried.push(format!("{git}: {e}"));
                continue;
            }
        };
        let paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|record| !record.is_empty())
            .map(str::to_string)
            .collect();
        let missing = missing_sentinels(&paths);
        if missing.is_empty() {
            return Ok(paths.into_iter().filter(|p| is_scanned_path(p)).collect());
        }
        tried.push(format!(
            "{git}: listed {} entries but not {missing:?} — output looks filtered",
            paths.len()
        ));
    }
    Err(format!(
        "no git could list this repo's index completely, so whether a leaked \
         credential sits in the docs cannot be answered. Tried:\n  {}",
        tried.join("\n  ")
    ))
}

/// Read every scannable tracked file. Skips unreadable files explicitly — an
/// encoding oddity must not silently scan as an empty (clean) document, so the
/// skip is recorded and surfaced in the summary instead.
fn load_sources(files: &[String]) -> (BTreeMap<String, String>, Vec<String>) {
    let mut sources = BTreeMap::new();
    let mut skipped = Vec::new();
    for relative in files {
        match std::fs::read_to_string(repo_root().join(relative)) {
            Ok(contents) => {
                sources.insert(relative.clone(), contents);
            }
            Err(_) => skipped.push(relative.clone()),
        }
    }
    (sources, skipped)
}

#[test]
fn no_credential_shaped_string_is_tracked_in_docs_or_config() {
    let files = match tracked_files() {
        Ok(files) => files,
        Err(why) => panic!("{why}"),
    };
    assert!(!files.is_empty(), "git reported no scannable tracked files");

    let (sources, skipped) = load_sources(&files);

    // Single pass over every tracked doc/config file through the pure detector.
    let mut findings = Vec::new();
    for (path, contents) in &sources {
        findings.extend(scan_doc(path, contents));
    }

    let skipped_note = if skipped.is_empty() {
        String::new()
    } else {
        format!(
            "\n(note: {} file(s) skipped as unreadable/non-UTF-8: {})",
            skipped.len(),
            skipped.join(", ")
        )
    };
    assert!(
        findings.is_empty(),
        "credential-shaped strings found in docs/config (CXA-B315) — the scrub \
         from CXA-B311a must not regress, and new leaks fail here with the rule, \
         file and line so the fix is obvious:{skipped_note}\n{}",
        findings
            .iter()
            .map(|f| format!("  {}:{} — {} [{}]", f.path, f.line, f.rule, f.snippet))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Negative direction, end to end over the REAL tree: take an actual tracked
/// doc, reintroduce a planted fake credential the way CXA-B031 leaked its DSN,
/// and prove the detector attributes it with rule + file + line — i.e. a fresh
/// leak cannot pass silently. Complements `reintroducing_the_old_literal_dsn_
/// shape_is_caught_with_file_and_line`, which holds the same assertion inside
/// the application layer.
#[test]
fn a_planted_fake_credential_in_a_real_doc_is_reported_with_file_and_line() {
    let files = match tracked_files() {
        Ok(files) => files,
        Err(why) => panic!("{why}"),
    };
    #[allow(clippy::expect_used)]
    let Some(victim) = files.first() else {
        panic!("the guard already proved tracked docs exist");
    };
    let original = std::fs::read_to_string(repo_root().join(victim)).unwrap();

    let planted_dsn = "postgres://coxagent:sK7vQm2xR9dLp4WtZ8nJhF3y@db.internal:5432/coxagent";
    let planted_line = format!("export COXAGENT_DB_DSN='{planted_dsn}'\n");
    let mut mutated = original.clone();
    mutated.push_str(&planted_line);

    let hits = scan_doc(victim, &mutated);
    assert_eq!(
        hits.len(),
        1,
        "exactly the planted line must be flagged: {hits:?}"
    );
    let hit = &hits[0];
    assert!(
        hit.rule.contains("connection URL") && !hit.snippet.contains("sK7vQm2xR9"),
        "finding names its rule and masks the value: {hit:?}"
    );
    // Line numbers count from the file top — this is what the failure message
    // renders as `{path}:{line}`, so a follow-up author lands on the leak.
    assert_eq!(hit.path, *victim);
    assert_eq!(hit.line, original.lines().count() + 1);

    // And the clean sibling still passes, closing both directions.
    assert!(scan_doc(victim, &original).is_empty());
}
