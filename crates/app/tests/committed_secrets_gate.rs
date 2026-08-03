//! COX-B030 regression guard: no live credential is committed to this repo.
//!
//! The bug: `deploy/.env` was tracked in git — header comment "Copy to .env and
//! fill in" and all — carrying `ADMIN_PASSWORD=<a real password>` that
//! docker-compose fed straight into `COXAGENT_ADMIN_PASSWORD`. It was not a
//! placeholder like its neighbours (`change-me-minio-secret`, …): posting it to
//! `/api/auth/login` on the running stack returned `200 {"role":"super"}` first
//! try. The same literal was compiled into `desktop/CoXAgentApp.swift`, so every
//! desktop install shipped with it, and copied into six Playwright specs.
//!
//! A code review catches this only if a reviewer happens to recognise a string
//! as a password. This gate does not need to: it enforces two rules that hold
//! regardless of what the value is.
//!
//!   1. **No `.env` is tracked.** `.env` is by definition the filled-in copy of
//!      `.env.example` — the file every deploy secret lands in. Whether today's
//!      value looks real is not the point; the file is the wrong place for git.
//!      This one rule covers every secret in it, not just the admin password.
//!   2. **No admin credential is a literal in tracked source.** An assignment to
//!      an admin-password key must read from the environment or be an obvious
//!      placeholder. A quoted string is a committed credential.
//!
//! Rule 2 deliberately does not try to tell "real" secrets from fake ones by
//! entropy or by matching the leaked value — a guard that hunts for one known
//! string only ever catches the leak that already happened, and embedding it
//! here would recommit it. The repo's own convention (`change-me-…`) is the
//! allowlist instead: say it is a placeholder, in the text, or read it from the
//! environment.
//!
//! `scan` is a pure function over `(path, contents)` pairs and returns findings
//! rather than panicking, so the synthetic cases at the bottom can prove the
//! guard bites before it is pointed at the real tree.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Assignment targets that name an admin login. Deliberately narrow: this
/// ticket is about the super-admin credential, and rule 1 already covers the
/// backing-service secrets wholesale by keeping `.env` out of git.
const ADMIN_KEYS: &[&str] = &["ADMIN_PASSWORD", "COXAGENT_ADMIN_PASSWORD", "password"];

fn has_extension(path: &str, ext: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Source, as opposed to deploy configuration. The distinction decides what
/// counts as a credential: a config file's values are all literal, while in
/// code only a quoted string is — everything else is an expression.
fn is_source_code(path: &str) -> bool {
    has_extension(path, "swift") || has_extension(path, "js")
}

/// Files rule 2 reads. Config that is committed on purpose, plus the two source
/// trees that had a copy of the credential. Everything else is covered by rule 1.
fn is_scanned_for_literals(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.starts_with(".env")
        || name.starts_with("docker-compose")
        || has_extension(path, "swift")
        || (path.starts_with("tests/") && has_extension(path, "js"))
}

/// Rule 1: `.env` is the filled-in copy, `.env.example` is the template.
fn is_secret_file(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or(path) == ".env"
}

/// A committed credential, named so the failure message points at the fix.
#[derive(Debug, PartialEq, Eq)]
struct Leak {
    path: String,
    line: usize,
    why: String,
}

/// Placeholders the repo already uses. Matched on a lowercased value so
/// `Change-Me` reads the same as `change-me`.
fn is_placeholder(value: &str) -> bool {
    let v = value.trim().to_ascii_lowercase();
    v.is_empty()
        || v.starts_with("change-me")
        || v.starts_with("change_me")
        || v.starts_with("changeme")
        || v.starts_with("your-")
        || v.starts_with('<')
}

/// A value the deploy resolves at run time rather than one baked into the file:
/// `${VAR}` in compose/env, a `secretKeyRef` in Helm.
fn is_env_reference(value: &str) -> bool {
    value.contains("${") || value.contains("secretKeyRef")
}

/// The value assigned to `key` on this line, and whether it was a quoted string
/// literal. Handles the four shapes the credential appeared in:
/// `KEY=v` (env), `KEY: v` (yaml), `env["KEY"] = "v"` (swift), `key: 'v'` (js).
///
/// Returns `None` when the key is not assigned here — a bare mention in prose,
/// a doc comment, or a read like `process.env.COXAGENT_ADMIN_PASSWORD`.
fn assigned_value<'a>(line: &'a str, key: &str) -> Option<(&'a str, bool)> {
    let at = find_word(line, key)?;
    let after = &line[at + key.len()..];
    // Step over the closing quote/bracket of a quoted key before the operator.
    let after = after.trim_start_matches(|c: char| "\"']} \t".contains(c));
    let value = after
        .strip_prefix('=')
        .or_else(|| after.strip_prefix(':'))?
        .trim_start();

    match value.chars().next() {
        // A quoted literal ends at its closing quote — trailing `, }` and the
        // rest of the line are not part of the secret.
        Some(q @ ('"' | '\'')) => {
            let rest = &value[q.len_utf8()..];
            let end = rest.find(q)?;
            Some((&rest[..end], true))
        }
        // Unquoted: in `.env`/compose that is the value; in code it is an
        // expression (`hubEnv[…] ?? adminPassword(in: ws)`), not a literal.
        _ => Some((value.trim_end(), false)),
    }
}

/// `line.find(key)`, but only where `key` is not part of a longer identifier —
/// so `PASSWORD` in `COXAGENT_ADMIN_PASSWORD` is left to that key's own entry.
fn find_word(line: &str, key: &str) -> Option<usize> {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(rel) = line[from..].find(key) {
        let at = from + rel;
        let before_ok = line[..at].chars().next_back().map_or(true, |c| !ident(c));
        let after_ok = line[at + key.len()..]
            .chars()
            .next()
            .map_or(true, |c| !ident(c));
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + key.len();
    }
    None
}

/// The whole COX-B030 check, over the tracked files git reports.
fn scan(files: &[(String, String)]) -> Vec<Leak> {
    let mut leaks = Vec::new();
    for (path, contents) in files {
        if is_secret_file(path) {
            leaks.push(Leak {
                path: path.clone(),
                line: 0,
                why: format!(
                    "`{path}` is tracked in git. `.env` holds filled-in deploy secrets — \
                     commit `.env.example` with placeholders instead, gitignore `.env`, \
                     and rotate every credential this file has ever held"
                ),
            });
            continue;
        }
        if !is_scanned_for_literals(path) {
            continue;
        }
        // In config every value is a literal; in code only a quoted one is.
        let config = !is_source_code(path);
        for (n, line) in contents.lines().enumerate() {
            if line.trim_start().starts_with('#') || line.trim_start().starts_with("//") {
                continue;
            }
            for key in ADMIN_KEYS {
                let Some((value, quoted)) = assigned_value(line, key) else {
                    continue;
                };
                if is_placeholder(value) || is_env_reference(value) {
                    continue;
                }
                // In code only a quoted string is a credential; anything else
                // is an expression evaluated at run time.
                if !quoted && !config {
                    continue;
                }
                leaks.push(Leak {
                    path: path.clone(),
                    line: n + 1,
                    why: format!(
                        "`{key}` is assigned a literal here. An admin password must come \
                         from the environment (`${{ADMIN_PASSWORD}}`, `process.env`, the \
                         generated per-install secret) — never from tracked source"
                    ),
                });
            }
        }
    }
    leaks
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
/// output-compressing `git` shim on `PATH` for agent runs, and it truncates
/// long listings (159 of 325 paths, when this gate was written). A gate handed
/// a truncated list reports "no credentials committed" about files it never
/// saw — the silent pass COX-B004 and COX-B025 were both about. So try the
/// real locations first and fall back to `PATH` only if none of them exist.
const GIT_CANDIDATES: &[&str] = &["/usr/bin/git", "/opt/homebrew/bin/git", "git"];

/// Paths this gate must be able to see. Every one of them is tracked and is
/// either a file the gate scans or the file that decides what git ignores; if
/// a listing is missing any of them, it is not a listing this gate can trust.
const SENTINELS: &[&str] = &[
    ".gitignore",
    "Cargo.toml",
    "README.md",
    "docker-compose.yml",
    "deploy/.env.example",
    "deploy/docker-compose.yml",
    "desktop/CoXAgentApp.swift",
    "tests/e2e.spec.js",
];

/// Which sentinels are absent from `files` — empty when the listing is whole.
fn missing_sentinels(files: &[String]) -> Vec<&'static str> {
    SENTINELS
        .iter()
        .filter(|s| !files.iter().any(|f| f == *s))
        .copied()
        .collect()
}

/// Every path git tracks. There is no way to answer "is this committed?"
/// without asking git, so an answer we cannot verify as complete fails the
/// gate rather than letting it pass on a short list.
fn tracked_files() -> Result<Vec<String>, String> {
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
        let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        let missing = missing_sentinels(&files);
        if missing.is_empty() {
            return Ok(files);
        }
        tried.push(format!(
            "{git}: listed {} paths but not {missing:?} — output looks filtered",
            files.len()
        ));
    }
    Err(format!(
        "no git could list this repo's tracked files completely, so whether a \
         credential is committed cannot be answered. Tried:\n  {}",
        tried.join("\n  ")
    ))
}

#[test]
fn no_credential_is_committed_to_this_repo() {
    let root = repo_root();
    let tracked = match tracked_files() {
        Ok(files) => files,
        Err(why) => panic!("{why}"),
    };
    let files: Vec<(String, String)> = tracked
        .into_iter()
        // Binary and deleted-but-still-listed paths simply read as nothing —
        // still listed, so rule 1 sees them; just with no lines to scan.
        .map(|p| {
            let body = std::fs::read_to_string(root.join(&p)).unwrap_or_default();
            (p, body)
        })
        .collect();
    assert!(!files.is_empty(), "git reported no tracked files at all");

    let leaks = scan(&files);
    assert!(
        leaks.is_empty(),
        "committed credentials (COX-B030):\n{}",
        leaks
            .iter()
            .map(|l| format!("  {}:{} — {}", l.path, l.line, l.why))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------------
// The guard, run against the bug it was written for — proves it fails when it
// should, and does not fire on the shapes the repo legitimately commits.
// ---------------------------------------------------------------------------

fn file(path: &str, body: &str) -> (String, String) {
    (path.to_string(), body.to_string())
}

#[test]
fn the_ticket_a_tracked_deploy_env_is_caught() {
    let leaks = scan(&[file(
        "deploy/.env",
        "# Copy to .env and fill in. All values are required.\n\
         ADMIN_USER=root\nADMIN_PASSWORD=Str0ngL00king\n",
    )]);
    assert_eq!(leaks.len(), 1, "expected exactly one finding: {leaks:?}");
    assert!(leaks[0].why.contains("tracked in git"), "{:?}", leaks[0]);
    assert!(
        leaks[0].why.contains("rotate"),
        "the fix is not just untracking: {:?}",
        leaks[0]
    );
}

#[test]
fn the_ticket_a_hard_coded_password_in_the_desktop_shell_is_caught() {
    let leaks = scan(&[file(
        "desktop/CoXAgentApp.swift",
        "        hubEnv[\"COXAGENT_ADMIN_USER\"] = \"root\"\n\
         \x20       hubEnv[\"COXAGENT_ADMIN_PASSWORD\"] = \"S3cretL1teral\"\n",
    )]);
    assert_eq!(leaks.len(), 1, "expected exactly one finding: {leaks:?}");
    assert_eq!(leaks[0].line, 2, "wrong line: {:?}", leaks[0]);
}

#[test]
fn the_ticket_a_hard_coded_password_in_a_spec_is_caught() {
    let leaks = scan(&[file(
        "tests/ui.spec.js",
        "    data: { username: 'root', password: 'S3cretL1teral' }\n",
    )]);
    assert_eq!(leaks.len(), 1, "expected exactly one finding: {leaks:?}");
}

#[test]
fn a_secret_file_is_caught_wherever_it_sits() {
    let leaks = scan(&[file(".env", "ADMIN_PASSWORD=whatever\n")]);
    assert_eq!(
        leaks.len(),
        1,
        "a root .env is as committed as a nested one"
    );
}

#[test]
fn the_fixed_shapes_pass() {
    let leaks = scan(&[
        file(
            "deploy/.env.example",
            "ADMIN_USER=root\nADMIN_PASSWORD=change-me-to-a-strong-password\n",
        ),
        file(
            "deploy/docker-compose.yml",
            "      COXAGENT_ADMIN_PASSWORD: ${ADMIN_PASSWORD}\n",
        ),
        file(
            "docker-compose.yml",
            "      COXAGENT_ADMIN_PASSWORD: ${COXAGENT_ADMIN_PASSWORD:-changeme}\n",
        ),
        file(
            "desktop/CoXAgentApp.swift",
            "        hubEnv[\"COXAGENT_ADMIN_PASSWORD\"] =\n\
             \x20           hubEnv[\"COXAGENT_ADMIN_PASSWORD\"] ?? adminPassword(in: ws)\n",
        ),
        file(
            "tests/ui.spec.js",
            "    data: { username: ADMIN_USER, password: ADMIN_PASSWORD }\n",
        ),
    ]);
    assert_eq!(leaks, vec![], "the shipped fix must not trip its own guard");
}

#[test]
fn prose_and_reads_are_not_assignments() {
    let leaks = scan(&[
        file(
            "desktop/CoXAgentApp.swift",
            "    // Set COXAGENT_ADMIN_PASSWORD to choose your own instead.\n\
             \x20   let pw = env[\"COXAGENT_ADMIN_PASSWORD\"]\n",
        ),
        file(
            "tests/credentials.js",
            "const ADMIN_PASSWORD = required('COXAGENT_ADMIN_PASSWORD');\n",
        ),
    ]);
    assert_eq!(
        leaks,
        vec![],
        "reading a variable is not committing a secret"
    );
}

#[test]
fn a_longer_key_is_not_matched_by_a_shorter_one_twice() {
    // `password` must not also fire inside `COXAGENT_ADMIN_PASSWORD`, or every
    // real finding would be reported twice and the count would lie.
    let leaks = scan(&[file(
        "deploy/.env",
        "COXAGENT_ADMIN_PASSWORD=S3cretL1teral\n",
    )]);
    assert_eq!(leaks.len(), 1, "one leak, reported once: {leaks:?}");
}

#[test]
fn a_truncated_file_listing_is_rejected_rather_than_scanned() {
    // The shim that shadows `git` on PATH here returned half the repo. Half a
    // repo scanned clean is not a clean repo, and the gate must say so instead
    // of reporting success over the files it happened to receive.
    let short = vec![".gitignore".to_string(), "Cargo.toml".to_string()];
    let missing = missing_sentinels(&short);
    assert!(
        missing.contains(&"deploy/.env.example") && missing.contains(&"desktop/CoXAgentApp.swift"),
        "a listing missing the files this gate scans must be flagged: {missing:?}"
    );
}

#[test]
fn a_whole_file_listing_is_accepted() {
    let whole: Vec<String> = SENTINELS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(missing_sentinels(&whole), Vec::<&str>::new());
}

#[test]
fn files_outside_the_scanned_set_are_left_alone() {
    // Rust test fixtures spin up their own throwaway containers; they are not
    // deploy configuration and this gate does not police them.
    let leaks = scan(&[file(
        "crates/app/tests/deploy_smoke.rs",
        "        .env(\"COXAGENT_ADMIN_PASSWORD\", \"ci-smoke\")\n",
    )]);
    assert_eq!(leaks, vec![], "only deploy config and the two source trees");
}
