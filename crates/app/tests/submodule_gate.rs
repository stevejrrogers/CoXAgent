//! CXA-B050 regression guard: no unresolvable gitlink is tracked in this repo.
//!
//! The bug: commit 9023969 ("feat(CXA-F026)") shipped, alongside two red
//! CXA-F024 gate tests that never compiled, a stray `mergetest` gitlink — a
//! mode-160000 index entry with no `.gitmodules` mapping. A gitlink without a
//! mapping is an unresolvable submodule: `git clone --recursive` and
//! `git submodule update --init` both abort on it with "no submodule mapping
//! found in .gitmodules", so every fresh-checkout build (the linux-build gate,
//! any CI or contributor clone) fails before cargo ever runs. The entry sat on
//! main through several merges because a normal clone neither checks it out
//! nor complains — only the recursive flows do.
//!
//! The same class leaked a second entry: an agent worktree committed as
//! `e2e/.coxagent-worktrees/default-feedback-449c37b4`. This gate enforces the
//! invariant for the whole tree, so the third instance fails CI instead of the
//! next fresh checkout.
//!
//! The rule: **every tracked gitlink must have a `.gitmodules` mapping.** A
//! repo that wants a submodule declares it; anything else in the index at mode
//! 160000 is debris.
//!
//! `scan` is a pure function over parsed `git ls-files -s` records and the
//! `.gitmodules` text, returning findings rather than panicking, so the
//! synthetic cases at the bottom prove the guard bites before it is pointed at
//! the real tree.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One `git ls-files -s` record: the index entry mode and its path.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    mode: String,
    path: String,
}

/// Git's gitlink mode: a submodule reference in the index.
const GITLINK: &str = "160000";

/// A tracked gitlink with no `.gitmodules` mapping.
#[derive(Debug, PartialEq, Eq)]
struct Finding {
    path: String,
}

/// Parse NUL-separated `git ls-files -s -z` records of the shape
/// `<mode> <object> <stage>\t<path>`. A record git could not have produced is
/// skipped rather than guessed at; the real-tree test fails loudly on an empty
/// parse, so wholesale format drift cannot read as a clean index.
fn parse_entries(raw: &str) -> Vec<Entry> {
    raw.split('\0')
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let (meta, path) = record.split_once('\t')?;
            let mode = meta.split(' ').next()?.to_string();
            Some(Entry {
                mode,
                path: path.to_string(),
            })
        })
        .collect()
}

/// The submodule paths a `.gitmodules` declares: every `path = X` line.
///
/// Section headers are not tracked — a `path` key only exists inside a
/// `[submodule …]` section, so flat key matching is sufficient here.
fn parse_gitmodules(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("path = "))
        .map(str::trim)
        .map(str::to_string)
        .collect()
}

/// The whole CXA-B050 check: every gitlink must be a declared submodule.
///
/// A path is reported once even when the index lists it several times — an
/// unresolved merge repeats it at stages 1/2/3, and a failure message naming
/// the same path three times reads as three problems.
fn scan(entries: &[Entry], declared: &BTreeSet<String>) -> Vec<Finding> {
    let mut seen = BTreeSet::new();
    entries
        .iter()
        .filter(|e| e.mode == GITLINK && !declared.contains(&e.path))
        .filter(|e| seen.insert(e.path.clone()))
        .map(|e| Finding {
            path: e.path.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The guard, run against the real tree.
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Where a real `git` lives, in preference order.
///
/// `Command::new("git")` alone is not safe here: this project installs an
/// output-compressing `git` shim on PATH for agent runs, and it truncates long
/// listings (74 of 592 index entries on the day this gate was written). A gate
/// handed a truncated index could report "no gitlinks" about entries it never
/// saw — the silent pass COX-B004 and COX-B025 were both about. So try the
/// real locations first and fall back to `PATH` only if none of them exist.
const GIT_CANDIDATES: &[&str] = &["/usr/bin/git", "/opt/homebrew/bin/git", "git"];

/// Paths the index must contain for the listing to be trusted. They span the
/// tree's areas, so a listing truncated by the PATH shim fails the sentinel
/// check instead of scanning clean over a fraction of the repo.
const SENTINELS: &[&str] = &[
    ".gitignore",
    "Cargo.toml",
    "docker-compose.yml",
    "deploy/docker-compose.yml",
    "e2e/package.json",
    "crates/app/tests/health_gate.rs",
];

/// Which sentinels are absent from `paths` — empty when the listing is whole.
fn missing_sentinels(paths: &[String]) -> Vec<&'static str> {
    SENTINELS
        .iter()
        .filter(|s| !paths.iter().any(|p| p == *s))
        .copied()
        .collect()
}

/// The staged index (`ls-files -s`), via a real git, or an error naming what
/// was tried. There is no way to answer "what does git track?" without asking
/// git, so an answer we cannot verify as complete fails rather than passing.
fn index_entries() -> Result<Vec<Entry>, String> {
    let root = repo_root();
    let mut tried = Vec::new();
    for git in GIT_CANDIDATES {
        let out = match Command::new(git)
            .arg("-C")
            .arg(&root)
            .args(["ls-files", "-s", "-z"])
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
        let entries = parse_entries(&String::from_utf8_lossy(&out.stdout));
        let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
        let missing = missing_sentinels(&paths);
        if missing.is_empty() {
            return Ok(entries);
        }
        tried.push(format!(
            "{git}: listed {} entries but not {missing:?} — output looks filtered",
            entries.len()
        ));
    }
    Err(format!(
        "no git could list this repo's index completely, so whether an \
         unresolvable gitlink is tracked cannot be answered. Tried:\n  {}",
        tried.join("\n  ")
    ))
}

#[test]
fn no_unresolvable_gitlink_is_tracked() {
    let entries = match index_entries() {
        Ok(entries) => entries,
        Err(why) => panic!("{why}"),
    };
    assert!(!entries.is_empty(), "git reported an empty index");

    let gitmodules = std::fs::read_to_string(repo_root().join(".gitmodules")).unwrap_or_default();
    let findings = scan(&entries, &parse_gitmodules(&gitmodules));
    assert!(
        findings.is_empty(),
        "unresolvable gitlinks tracked (CXA-B050) — a fresh `git clone \
         --recursive` aborts on these; untrack them (`git rm --cached`) and \
         gitignore the path, or declare them in `.gitmodules`:\n{}",
        findings
            .iter()
            .map(|f| format!("  {} (mode 160000, no .gitmodules mapping)", f.path))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------------
// The parsers, against the record shapes git actually emits.
// ---------------------------------------------------------------------------

/// One real record per index entry, NUL-terminated — gitlinks and blobs alike.
#[test]
fn parse_reads_mode_and_path_from_real_records() {
    let entries = parse_entries(
        "160000 ddf94e68294c3f8caa33f7271add901ac1fb2387 0\tmergetest\0\
         100644 e9ecdbafb169864886d9f001e8dbcefebcde5d27 0\t.gitignore\0",
    );
    assert_eq!(
        entries,
        vec![entry("160000", "mergetest"), entry("100644", ".gitignore")]
    );
}

/// The stage column is not part of the mode: a conflicted path appears at
/// stages 1/2/3 with the same mode, which must still read as a gitlink.
#[test]
fn parse_keeps_the_mode_clean_of_the_stage_column() {
    let entries = parse_entries(
        "160000 ddf94e68294c3f8caa33f7271add901ac1fb2387 2\tmergetest\0\
         160000 bf41249e2c29635566017b4b5c3b07c1492e33c2 3\tmergetest\0",
    );
    assert_eq!(entries.len(), 2, "one record per stage: {entries:?}");
    assert!(entries.iter().all(|e| e.mode == "160000"), "{entries:?}");
}

/// Trailing NULs and records without a TAB are not entries.
#[test]
fn parse_skips_empty_and_garbage_records() {
    // Split into two literals so the NUL is never followed by digits in
    // source — `\0` + `100644` reads as an octal escape even though it is
    // a terminator plus the next record's mode.
    let raw = "\0\0no-tab-here\0".to_owned() + "100644 abc 0\tok\0";
    let entries = parse_entries(&raw);
    assert_eq!(entries, vec![entry("100644", "ok")]);
}

// ---------------------------------------------------------------------------
// The guard, run against the bug it was written for — proves it fails when it
// should, and does not fire on the shapes the repo legitimately tracks.
// ---------------------------------------------------------------------------

fn entry(mode: &str, path: &str) -> Entry {
    Entry {
        mode: mode.to_string(),
        path: path.to_string(),
    }
}

/// The exact damage from 9023969: a stray root gitlink, no `.gitmodules`.
#[test]
fn the_ticket_a_stray_mergetest_gitlink_is_caught() {
    let findings = scan(&[entry("160000", "mergetest")], &BTreeSet::new());
    assert_eq!(findings.len(), 1, "expected exactly one finding: {findings:?}");
    assert_eq!(findings[0].path, "mergetest");
}

/// The second instance: an agent worktree committed as a gitlink.
#[test]
fn a_leaked_worktree_gitlink_is_caught() {
    let findings = scan(
        &[entry("160000", "e2e/.coxagent-worktrees/default-feedback-449c37b4")],
        &BTreeSet::new(),
    );
    assert_eq!(findings.len(), 1, "expected exactly one finding: {findings:?}");
}

/// A declared submodule is the legitimate shape and passes untouched.
#[test]
fn a_declared_submodule_passes() {
    let declared = parse_gitmodules(
        "[submodule \"vendor/lib\"]\n\tpath = vendor/lib\n\turl = ../lib\n",
    );
    let findings = scan(
        &[entry("160000", "vendor/lib"), entry("100644", "vendor/lib/README")],
        &declared,
    );
    assert_eq!(findings, vec![], "a mapped submodule is not debris");
}

/// Regular blobs and executables never fire, whatever their path.
#[test]
fn ordinary_files_are_left_alone() {
    let findings = scan(
        &[
            entry("100644", "mergetest"),
            entry("100755", "deploy/self-upgrade.sh"),
            entry("120000", "some/symlink"),
        ],
        &BTreeSet::new(),
    );
    assert_eq!(findings, vec![], "only mode 160000 is a gitlink");
}

/// `path =` keys under any submodule section all map. Flat matching is
/// deliberately loose: under-reporting a declared submodule fails the gate
/// closed on a legitimate entry — it can never silently pass a gitlink.
#[test]
fn gitmodules_parsing_reads_every_declared_path() {
    let declared = parse_gitmodules(
        "[submodule \"a\"]\n\tpath = vendor/a\n\turl = ../a\n\
         [submodule \"b\"]\n\tpath = vendor/b\n",
    );
    assert_eq!(
        declared,
        BTreeSet::from(["vendor/a".to_string(), "vendor/b".to_string()])
    );
}

/// An unresolved merge lists the same path at stages 1/2/3 — one problem, one
/// finding, not three.
#[test]
fn conflicted_stage_entries_report_one_finding() {
    let stages = [
        entry("160000", "mergetest"),
        entry("160000", "mergetest"),
        entry("160000", "mergetest"),
    ];
    let findings = scan(&stages, &BTreeSet::new());
    assert_eq!(findings.len(), 1, "one path, reported once: {findings:?}");
}

#[test]
fn a_truncated_index_listing_is_rejected_rather_than_scanned() {
    // The shim that shadows `git` on PATH here returned 74 of 592 entries. A
    // fraction of the index scanned clean is not a clean index, and the gate
    // must say so instead of reporting success over what it happened to see.
    let short: Vec<String> = vec![".gitignore".to_string(), "Cargo.toml".to_string()];
    let missing = missing_sentinels(&short);
    assert!(
        missing.contains(&"e2e/package.json") && missing.contains(&"deploy/docker-compose.yml"),
        "a listing missing whole areas must be flagged: {missing:?}"
    );
}

#[test]
fn a_whole_index_listing_is_accepted() {
    let whole: Vec<String> = SENTINELS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(missing_sentinels(&whole), Vec::<&str>::new());
}
