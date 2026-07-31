//! The hexagonal ratchet: the application layer does not do IO directly.
//!
//! Every external effect — a process, a file, a socket — goes through a port
//! in `ports/outbound/`, fulfilled by an infrastructure adapter. Direct
//! `std::process`/`std::fs` in a use case does more than bend the rule: it
//! makes the logic untestable without a real repo/filesystem, which is where
//! this workspace's flaky tests came from.
//!
//! Files that predate the rule are grandfathered below. The list may only
//! SHRINK: fixing a file and forgetting to remove it here fails the test too,
//! so the debt stays visible and the ratchet only turns one way. A NEW file
//! reaching for std::process/std::fs fails immediately, with this explanation.

use std::path::{Path, PathBuf};

/// Files that already did direct IO when the ratchet was installed
/// (2026-07-31). Remove an entry when you fix the file — never add one.
const GRANDFATHERED: &[&str] = &[
    "cleanup.rs",
    "codegraph.rs",
    "conformance.rs",
    "prompts.rs",
    "use_cases/cycle/qa_evidence.rs",
    "use_cases/run_reviews.rs",
    "verify_cache.rs",
];

const FORBIDDEN: &[&str] = &["std::process::Command", "std::fs::"];

fn application_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../application/src");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p: PathBuf = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p
                    .strip_prefix(&root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned();
                if let Ok(text) = std::fs::read_to_string(&p) {
                    out.push((rel, text));
                }
            }
        }
    }
    out.sort();
    out
}

/// Only production code counts: `#[cfg(test)]` modules may do IO (a test
/// double that shells out is test scaffolding, not architecture).
fn production_half(text: &str) -> &str {
    text.split("#[cfg(test)]").next().unwrap_or(text)
}

#[test]
fn application_does_io_through_ports_only() {
    let mut new_offenders = Vec::new();
    let mut fixed_but_listed = Vec::new();
    for (rel, text) in application_sources() {
        let dirty = FORBIDDEN.iter().any(|f| production_half(&text).contains(f));
        let listed = GRANDFATHERED.contains(&rel.as_str());
        match (dirty, listed) {
            (true, false) => new_offenders.push(rel),
            (false, true) => fixed_but_listed.push(rel),
            _ => {}
        }
    }
    assert!(
        new_offenders.is_empty(),
        "these application files reach for std::process/std::fs directly — put the IO behind a \
         port in ports/outbound/ (see GitPort::working_tree for the pattern) and keep the \
         decision a pure function: {new_offenders:?}"
    );
    assert!(
        fixed_but_listed.is_empty(),
        "these files no longer do direct IO — remove them from GRANDFATHERED so the ratchet \
         keeps turning: {fixed_but_listed:?}"
    );
}
