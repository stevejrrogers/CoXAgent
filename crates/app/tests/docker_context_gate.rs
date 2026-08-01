//! COX-B030 regression guard: the docker build context must not carry build
//! artefacts or filled-in secrets.
//!
//! The bug: `.dockerignore` listed `target/`. Docker anchors a pattern with no
//! `**` at the context root, so that one line excludes `./target` and nothing
//! else. The per-agent git worktrees under `.claude/worktrees/` are whole
//! checkouts with their own `target/` — 12 GB of them on the machine that hit
//! this — and every byte was sent to the builder and unpacked there. The
//! release build then died as `exit code: 101` with no rustc diagnostic at all,
//! which reads like a compile error and is not one. That is what rejected this
//! ticket's first attempt at the `linux-build` gate.
//!
//! The second half is the ticket proper: `.env` holds the live admin password
//! and every backing-service credential. Untracking it from git (which this
//! ticket did) does not keep it out of an image — `COPY . .` would bake the
//! developer's filled-in copy into a layer that ships.
//!
//! The decision is `is_excluded`, a pure function over the `.dockerignore` body
//! and a context-relative path. The tests at the bottom feed it synthetic
//! bodies to prove it bites: they show the old root-anchored `target/` failing
//! on a nested path, so the guard cannot pass by accident.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Does `pattern` match `path`, segment by segment? `**` matches any run of
/// segments (including none), `*` matches within one segment.
fn matches(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        // Every pattern segment consumed: a match, whatever is left of the
        // path — docker excludes a directory's whole subtree.
        None => true,
        Some((&"**", rest)) => (0..=path.len()).any(|skip| matches(rest, &path[skip..])),
        Some((head, rest)) => match path.split_first() {
            Some((segment, tail)) if segment_matches(head, segment) => matches(rest, tail),
            _ => false,
        },
    }
}

/// One path segment against one pattern segment, where `*` matches any run of
/// characters that does not cross a `/` (segments never contain one).
fn segment_matches(pattern: &str, segment: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = segment.strip_prefix(first) else {
        return false;
    };
    let mut parts = parts.peekable();
    if parts.peek().is_none() {
        return rest.is_empty();
    }
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            // Trailing literal must land at the end (a bare trailing `*`
            // is the empty string, which every tail ends with).
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

/// Would docker exclude `path` from the build context, given this
/// `.dockerignore` body? Later patterns win over earlier ones, and a leading
/// `!` re-includes — the same last-match-wins rule docker itself applies.
fn is_excluded(dockerignore: &str, path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut excluded = false;
    for line in dockerignore.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, pattern) = match line.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, line),
        };
        let pattern: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
        if matches(&pattern, &segments) {
            excluded = !negated;
        }
    }
    excluded
}

/// Paths that must never reach the builder, and why each one matters.
const MUST_BE_EXCLUDED: &[(&str, &str)] = &[
    ("target/release/coxagent", "the host's own build artefacts"),
    (
        ".claude/worktrees/cox-b059/target/release/coxagent",
        "a per-agent worktree's target/ — 12 GB of these sank the release build",
    ),
    (
        ".claude/worktrees/cox-b059/crates/app/src/lib.rs",
        "a per-agent worktree at all: it is a second copy of the whole repo",
    ),
    (
        "deploy/.env",
        "the filled-in deploy secrets — the admin password lives here (COX-B030)",
    ),
    (".env", "a root .env is as live as a nested one"),
    (
        "admin-password",
        "the desktop shell's generated per-install admin password",
    ),
    (
        ".git/config",
        "repository metadata the build has no use for",
    ),
];

/// Paths the build genuinely needs, or that document the secrets without
/// being one — an over-broad rule that swallowed these would break the build
/// or the image, so the guard pins them open.
const MUST_BE_INCLUDED: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "crates/app/src/lib.rs",
    "docker/entrypoint.sh",
    "deploy/.env.example",
    "web/js/app.js",
];

#[test]
fn the_docker_context_carries_no_artefacts_and_no_secrets() {
    let body = std::fs::read_to_string(repo_root().join(".dockerignore")).unwrap();
    let leaks: Vec<String> = MUST_BE_EXCLUDED
        .iter()
        .filter(|(path, _)| !is_excluded(&body, path))
        .map(|(path, why)| format!("  {path} — {why}"))
        .collect();
    assert!(
        leaks.is_empty(),
        ".dockerignore lets these into the build context:\n{}",
        leaks.join("\n")
    );
}

#[test]
fn the_docker_context_still_carries_what_the_build_needs() {
    let body = std::fs::read_to_string(repo_root().join(".dockerignore")).unwrap();
    let missing: Vec<&str> = MUST_BE_INCLUDED
        .iter()
        .copied()
        .filter(|path| is_excluded(&body, path))
        .collect();
    assert!(
        missing.is_empty(),
        ".dockerignore excludes files the build needs: {missing:?}"
    );
}

// ---------------------------------------------------------------------------
// The matcher, run against the bug it was written for.
// ---------------------------------------------------------------------------

#[test]
fn the_ticket_a_root_anchored_pattern_misses_a_nested_directory() {
    // Exactly the `.dockerignore` that sent 12 GB to the builder.
    assert!(is_excluded("target/\n", "target/release/coxagent"));
    assert!(!is_excluded(
        "target/\n",
        ".claude/worktrees/cox-b059/target/release/coxagent"
    ));
    // ...and the fix.
    assert!(is_excluded(
        "**/target/\n",
        ".claude/worktrees/cox-b059/target/release/coxagent"
    ));
}

#[test]
fn a_directory_pattern_excludes_its_whole_subtree() {
    assert!(is_excluded(".claude/\n", ".claude/worktrees/x/Cargo.toml"));
    assert!(!is_excluded(".claude/\n", "crates/app/Cargo.toml"));
}

#[test]
fn a_negation_re_includes_and_the_last_match_wins() {
    let body = "**/.env\n!**/.env.example\n";
    assert!(is_excluded(body, "deploy/.env"));
    assert!(!is_excluded(body, "deploy/.env.example"));
    // Order matters: negate first and the later exclusion takes it back.
    assert!(is_excluded("!**/.env.example\n**/.env\n", "deploy/.env"));
}

#[test]
fn a_star_stays_inside_one_segment() {
    assert!(is_excluded("**/*.dmg\n", "dist/CoXAgent.dmg"));
    assert!(!is_excluded("**/*.dmg\n", "dist/CoXAgent.dmg.sha256"));
    assert!(!is_excluded("*.dmg\n", "dist/CoXAgent.dmg"));
}

#[test]
fn comments_and_blank_lines_are_not_patterns() {
    let body = "# target/\n\n   \n";
    assert!(!is_excluded(body, "target/release/coxagent"));
}
