//! CXA-B314 regression guard: watchdog scripts must never inline a datastore
//! credential.
//!
//! The bug: the three helper scripts under `.cox-watchdog/` carried the live
//! Postgres password and a credentialed Redis URL as string literals:
//!
//! ```python
//! PG = dict(host='127.0.0.1', port=5433, user='coxagent',
//!           password='Wks8…', dbname='coxagent')
//! REDIS_URL = 'redis://:HQz6…@127.0.0.1:6379/0'
//! ```
//!
//! They were tracked in git, so every clone carried deploy-local database
//! credentials. CXA-B314 scrubbed them: each script now reads
//! `PG_PASSWORD` / `REDIS_PASSWORD` from the environment (the same sanctioned
//! source `docker_compose.rs` resolves for deploys).
//!
//! Like its sibling gates (`committed_secrets_gate`, `compose_security_gate`)
//! this does NOT hunt for the one string that already leaked — a guard pinned
//! to a known literal only ever catches the leak that already happened, and
//! echoing it into a tracked test would un-do the fix's own "zero hits"
//! property. It enforces two structural rules that hold regardless of value,
//! over `(path, contents)` pairs:
//!
//!   1. **No credentialed literal.** In a `.cox-watchdog/*.py` script, a
//!      non-empty quoted value assigned to a postgres `password=` key, or a
//!      `redis://:<secret>@host` URL, is a committed credential — unless the
//!      value is an interpolation hole (`%s`, `{}`), which by construction
//!      means the secret came from somewhere else at run time.
//!   2. **Credentials are read from the environment.** Each script must
//!      obtain `PG_PASSWORD` and `REDIS_PASSWORD` via `os.environ.get(...)`
//!      (or `os.getenv(...)`). Rule 1 alone would let a script drop the
//!      password entirely — a silent behaviour change, not a fix.
//!
//! `scan` is pure; the synthetic cases at the bottom replay the exact pre-fix
//! code shapes (with stand-in literals, so the real values are not recommitted)
//! to prove the guard fails on the unfixed state, and the whole-tree case runs
//! it against the live scripts.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

/// A structural violation in a watchdog script, worded so the failure points
/// at the sanctioned source of the secret.
#[derive(Debug, PartialEq, Eq)]
struct Leak {
    path: String,
    line: usize,
    why: String,
}

fn is_watchdog_script(path: &str) -> bool {
    path.starts_with(".cox-watchdog/") && path.ends_with(".py")
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('#') || t.starts_with("//")
}

/// True when `value` is not itself the secret — an interpolation hole fed from
/// the environment (`'redis://:%s@…' % os.environ.get(…)`) rather than the
/// secret baked in.
fn is_interpolation_hole(value: &str) -> bool {
    value.contains("%s") || value.contains('{') || value.contains("<")
}

/// Extract the inside of a quoted Python string starting at `open_q`.
/// Returns the body up to the matching closing quote of the SAME kind.
fn quoted_body(rest: &str, open_q: char) -> Option<&str> {
    let body = rest.strip_prefix(open_q)?;
    let end = body.find(open_q)?;
    Some(&body[..end])
}

/// The secret embedded in a `redis://:SECRET@host/…` URL, if the URL carries one.
/// Empty password (no auth) yields `None`.
fn redis_url_secret(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("redis://:")?;
    let at = rest.find('@')?;
    let secret = &rest[..at];
    (!secret.is_empty()).then_some(secret)
}

/// Rule 1 for one line: a postgres password or a Redis URL carrying the
/// credential inline.
fn literal_leak(line: &str) -> Option<String> {
    // Postgres: `password='<quoted>'` inside the connection dict/kwargs. Only
    // an actual `password=` assignment counts — the word inside another quoted
    // key (e.g. `'pg_password'` in a keyword list) does not. The value must be
    // either empty-by-design (`''` lets psycopg2 fall through to
    // PGPASSWORD/.pgpass) or a run-time expression — see rule 2's message.
    let pw_assign = ["password='", "password=\""].iter().find_map(|needle| {
        line.find(needle)
            .map(|at| (&line[at + needle.len() - 1..], needle.as_bytes()[9] as char))
    });
    if let Some((after, q)) = pw_assign {
        let value = quoted_body(after, q).unwrap_or("");
        let rest = &after[after.find(q).map(|e| e + 1).unwrap_or(0)..];
        let env_read = rest.contains("os.environ")
            || rest.contains("os.getenv")
            || value.is_empty()
            || is_interpolation_hole(value);
        if !env_read {
            return Some(format!(
                "a postgres password literal is committed here. Read it from \
                 the environment instead: `os.environ.get('PG_PASSWORD', '')` \
                 (exported from the sanctioned deploy-secrets store)"
            ));
        }
    }
    // Redis: `REDIS_URL = 'redis://:SECRET@host/…'`. An interpolated hole
    // (`%s` filled from os.environ) is the fixed shape; anything else with a
    // secret between `:` and `@` is the credential, committed.
    if let Some(at) = line.find("redis://:") {
        let tail = &line[at..];
        let q = tail.chars().find(|c| *c == '"' || *c == '\'');
        if let Some(q) = q {
            let url = quoted_body(&tail[tail.find(q).unwrap_or(0)..], q).unwrap_or(tail);
            if let Some(secret) = redis_url_secret(url) {
                if !is_interpolation_hole(secret) {
                    return Some(format!(
                        "a credentialed Redis URL literal is committed here. Build it \
                         from the environment at run time instead: \
                         `'redis://:%s@host/db' % os.environ.get('REDIS_PASSWORD', '')`"
                    ));
                }
            }
        }
    }
    None
}

/// Rule 2 markers: every watchdog script touching Postgres/Redis must source
/// its credentials from the environment under these names.
const ENV_KEYS: &[&str] = &["PG_PASSWORD", "REDIS_PASSWORD"];

/// The whole CXA-B314 check, over watchdog script contents.
fn scan(files: &[(String, String)]) -> Vec<Leak> {
    let mut leaks = Vec::new();
    for (path, contents) in files {
        if !is_watchdog_script(path) {
            continue;
        }
        for (n, line) in contents.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if let Some(why) = literal_leak(line) {
                leaks.push(Leak {
                    path: path.clone(),
                    line: n + 1,
                    why,
                });
            }
        }
        for key in ENV_KEYS {
            let sourced = contents
                .lines()
                .any(|l| l.contains(key) && (l.contains("os.environ") || l.contains("os.getenv")));
            if !sourced {
                leaks.push(Leak {
                    path: path.clone(),
                    line: 0,
                    why: format!(
                        "`{key}` is never read from the environment here. Credentials \
                         must come from `os.environ.get('{key}', '')` (the sanctioned \
                         deploy-secrets source), never from a tracked literal or a \
                         dropped default"
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

/// Every tracked watchdog script, read from disk.
fn watchdog_scripts() -> Vec<(String, String)> {
    let dir = repo_root().join(".cox-watchdog");
    let mut found = Vec::new();
    for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
    {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".py") {
            let body = fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
            found.push((format!(".cox-watchdog/{name}"), body));
        }
    }
    found.sort();
    found
}

#[test]
fn no_datastore_credential_is_inlined_in_watchdog_scripts() {
    let scripts = watchdog_scripts();
    assert!(
        !scripts.is_empty(),
        "no .cox-watchdog scripts found — the gate saw nothing and cannot vouch"
    );
    let leaks = scan(&scripts);
    assert!(
        leaks.is_empty(),
        "inlined watchdog credentials (CXA-B314):\n{}",
        leaks
            .iter()
            .map(|l| format!("  {}:{} — {}", l.path, l.line, l.why))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------------
// The guard, run against the bug it was written for. The fixtures below replay
// the exact pre-CXA-B314 code shapes; the literals are STAND-INS (never the
// values that leaked) so the fix's own zero-literals property stays intact.
// ---------------------------------------------------------------------------

fn file(path: &str, body: &str) -> (String, String) {
    (path.to_string(), body.to_string())
}

/// The pre-fix shape of every one of the three scripts, byte-for-byte apart
/// from the stand-in credentials.
const PRE_FIX_STATE_PY: &str = "\
#!/usr/bin/env python3
\"\"\"Watchdog helper: dump live agent state (Postgres activity + Redis worker).\"\"\"
import json
import psycopg2
import redis

PG = dict(host='127.0.0.1', port=5433, user='coxagent',
          password='StandInPgPasswordNotTheRealOne', dbname='coxagent')
REDIS_URL = 'redis://:StandInRedisPasswordNotTheRealOne@127.0.0.1:6379/0'
WORKER_KEY = 'cox:cxa:worker:root@example-host'
";

#[test]
fn the_unfixed_script_fails_every_rule_of_the_guard() {
    let leaks = scan(&[file(".cox-watchdog/state.py", PRE_FIX_STATE_PY)]);
    assert_eq!(
        leaks.len(),
        4,
        "expected pg literal + redis literal + one missing-env finding per key: {leaks:?}"
    );
    assert!(
        leaks.iter().all(|l| l.why.contains("environment")),
        "every finding must point at the sanctioned env source: {leaks:?}"
    );
    assert!(
        leaks
            .iter()
            .any(|l| l.why.contains("postgres password literal")),
        "{leaks:?}"
    );
    assert!(
        leaks
            .iter()
            .any(|l| l.why.contains("credentialed Redis URL literal")),
        "{leaks:?}"
    );
}

#[test]
fn the_same_bug_in_monitor_and_state2_is_also_caught() {
    for path in [".cox-watchdog/monitor_b051.py", ".cox-watchdog/state2.py"] {
        let leaks = scan(&[file(path, PRE_FIX_STATE_PY)]);
        assert_eq!(
            leaks.len(),
            4,
            "{path} shares the pre-fix shape and must be caught: {leaks:?}"
        );
    }
}

#[test]
fn dropping_the_password_entirely_is_not_a_fix() {
    // Silently removing the credential would satisfy rule 1 but break the
    // script; rule 2 must catch it.
    let body = "\
import json
import psycopg2
import redis

PG = dict(host='127.0.0.1', port=5433, user='coxagent', dbname='coxagent')
";
    let leaks = scan(&[file(".cox-watchdog/state.py", body)]);
    assert_eq!(leaks.len(), 2, "one missing-env finding per key: {leaks:?}");
    assert!(
        leaks
            .iter()
            .all(|l| l.why.contains("never read from the environment")),
        "{leaks:?}"
    );
    assert!(
        leaks[0].why.contains("PG_PASSWORD"),
        "must name the missing env key: {:?}",
        leaks[0]
    );
}

#[test]
fn the_fixed_shape_is_clean() {
    // The exact post-fix lines, as shipped by CXA-B314.
    let body = "\
import json
import os
import psycopg2
import redis

# Credentials are NEVER inlined here (CXA-B031 hygiene): read them from the
# environment — export PG_PASSWORD / REDIS_PASSWORD from the sanctioned secret
# source before running.
PG = dict(host='127.0.0.1', port=5433, user='coxagent',
          password=os.environ.get('PG_PASSWORD', ''), dbname='coxagent')
REDIS_URL = 'redis://:%s@127.0.0.1:6379/0' % os.environ.get('REDIS_PASSWORD', '')
WORKER_KEY = 'cox:cxa:worker:root@example-host'
";
    let leaks = scan(&[file(".cox-watchdog/state.py", body)]);
    assert_eq!(leaks, vec![], "the shipped fix must not trip its own guard");
}

#[test]
fn empty_pg_password_placeholder_still_requires_env_reads() {
    // `password=''` falls through to PGPASSWORD/.pgpass — legal, but rule 2
    // still demands the env read so behaviour stays explicit.
    let body = "\
import psycopg2
import redis

PG = dict(host='h', password='', dbname='coxagent')
REDIS_URL = 'redis://:@h:6379/0'
";
    let leaks = scan(&[file(".cox-watchdog/state.py", body)]);
    assert_eq!(leaks.len(), 2, "one missing-env finding per key: {leaks:?}");
    assert!(leaks.iter().all(|l| l.line == 0), "{leaks:?}");
}

#[test]
fn prose_mentions_are_not_assignments() {
    // A doc comment naming PG_PASSWORD (like the CXA-B031 note in the fixed
    // scripts) must not be mistaken for the required env read.
    let body = "\
# export PG_PASSWORD and REDIS_PASSWORD before running.
PG = dict(password=os.environ.get('PG_PASSWORD', ''))
REDIS_URL = 'redis://:%s@h/0' % os.environ.get('REDIS_PASSWORD', '')
";
    let leaks = scan(&[file(".cox-watchdog/state.py", body)]);
    assert_eq!(leaks, vec![]);
}
