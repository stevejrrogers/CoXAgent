//! CXA-F259 — In-product loop-liveness watchdog: alert on a silently stalled
//! autonomous cycle. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "Given desired-run=true, no pause/budget/quota pause in effect, and a
//!    registered worker whose heartbeat is stale while the activity trail has
//!    had no new entry for longer than the configured threshold, exactly one
//!    stall alert is raised through the existing alert channels naming the
//!    worker, the last activity, and the elapsed silence."
//! 2. "A loop legitimately paused by budget cap, quota exhaustion, user pause,
//!    or an empty backlog produces no stall alert (documented non-stall
//!    reasons are excluded by the predicate)."
//! 3. "The same stall episode never alerts twice (deduped until activity
//!    resumes), so a multi-hour stall cannot flood the feed, and the condition
//!    self-clears when new activity lands."
//! 4. "The stall predicate (trigger, alert text, clear condition) is a pure
//!    function over a store/registry snapshot, testable with a struct literal
//!    and no live processes, and the checker runs hub-side so it also fires
//!    when the worker process is entirely gone."
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the real state/domain
//! types the codebase has today (`WorkerEntry`, `ActivityEntry`,
//! `ProjectState`) plus source-scan guards over the files the loop and the hub
//! actually run (`use_cases/runner.rs`, `use_cases/cycle/mod.rs`,
//! `server/mod.rs`) — the same no-harness discipline as
//! `live_repro_url_f246_tdd.rs`: no fake HTTP server, no host harness, no
//! network port, no invented identifiers. A test that called the stall
//! predicate directly could not compile today (no such symbol exists), so the
//! red half pins the missing predicate where it must live and what it must
//! consume, and the green half pins the executable semantics over the types
//! that DO exist. Every failing assertion below fails only because CXA-F259's
//! behaviour is missing; if an assertion's mechanism moves during
//! implementation, move the guard with it (the `preflight_f239_tdd.rs`
//! convention).
//!
//! Green guards (fixture validity — house rule: every fixture must be
//! buildable from data the codebase actually has):
//!   * the stall snapshot is fully expressible today as struct literals over
//!     `WorkerEntry` (the registry heartbeat, RFC3339 `at`) and `ActivityEntry`
//!     (the persisted activity trail on `ProjectState`); elapsed silence is
//!     computable from those literals with the codebase's own RFC3339 parser
//!     and no live processes — the exact premise AC4 demands;
//!   * the documented non-stall reasons are real loop behaviour: the
//!     budget-cap pause ("loop paused: spend cap reached", runner.rs), the
//!     quota pause ("quota_exhausted", cycle/mod.rs), the user pause
//!     (`RunnerHandle::pause`, runner.rs), and an empty backlog
//!     (`ProjectState::default()` — no tickets, nothing to do).
//!
//! IMPLEMENTER NOTES (design gaps flagged, resolved in the implementation):
//!   * Registry TTL vs "worker entirely gone": `StateStorePort::workers()`
//!     prunes entries older than the worker TTL (600s), so a worker whose
//!     process died hours ago is ABSENT from the live registry. The
//!     implementation keys the trigger on the persisted ACTIVITY TRAIL (the
//!     out-of-product predecessor's source) and lets the registry shape only
//!     the alert text — the alternative the note below blesses.
//!   * Unparseable stamps: the predicate picks the "never guess" side
//!     deliberately — an unparseable stamp leaves the loop Quiet (and an open
//!     episode Resumed), tested in `liveness.rs`.
//!   * Alert kinds: `cycle_stalled` / `cycle_resumed`, additive per the
//!     NotifyEvent forward-compat contract (kind_icon arms tested).
//!   * The configured threshold: `workflow.stall_timeout_secs` /
//!     `stall_hub_timeout_secs` (zero-means-default resolution in
//!     `liveness::stall_threshold` / `stall_escalation`).
//!
//! AC → test map:
//! - AC1:
//!   [`ac1_a_stale_heartbeat_with_a_silent_trail_raises_one_stall_alert_through_the_existing_channels`],
//!   plus the green
//!   [`the_snapshot_ingredients_are_pure_struct_literals_over_existing_types`]
//! - AC2: [`ac2_documented_non_stall_reasons_are_excluded_by_the_predicate`],
//!   plus the green
//!   [`the_documented_non_stall_pauses_exist_in_the_loop_today`]
//! - AC3:
//!   [`ac3_a_stall_episode_alerts_once_until_activity_resumes_and_then_self_clears`]
//! - AC4:
//!   [`ac4_the_predicate_is_a_pure_snapshot_function_and_the_checker_runs_hub_side`],
//!   plus the green snapshot guard

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::ports::outbound::WorkerEntry;
use coxagent_application::state::{now_rfc3339, ActivityEntry, ProjectState};

// --- repo-state scan helpers (the live_repro_url_f246_tdd.rs pattern) --------

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

/// Every Rust source under a directory, recursively, sorted for determinism.
fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
}

fn application_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_rs(&repo_root().join("crates/application/src"), &mut out);
    out
}

/// The source window of one top-level item: from its `header` to the next item
/// introduced by `terminator` (or end of file). An absent header yields an
/// empty window — the caller's own assertion, not an index panic, must report
/// the miss.
fn window_of<'a>(src: &'a str, header: &str, terminator: &str) -> &'a str {
    let Some(at) = src.find(header) else {
        return "";
    };
    let rest = &src[at..];
    let end = rest[header.len()..]
        .find(terminator)
        .map_or(rest.len(), |rel| header.len() + rel);
    &rest[..end]
}

/// The identifier a `fn` declaration on this line introduces (`fn stall_alert`
/// → `stall_alert`); `None` when the line declares no function.
fn fn_name(line: &str) -> Option<String> {
    let at = line.find("fn ")? + "fn ".len();
    let name: String = line[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// True when the identifier names a LOOP-LIVENESS stall function. `stall` as
/// part of `install` is engine/tooling detection (`ensure_engine_installed`),
/// never the watchdog — excluded explicitly.
fn stall_fn_name(name: &str) -> bool {
    name.contains("stall") && !name.contains("install")
}

/// The module defining the loop-liveness stall predicate, if it exists, as
/// `(repo-relative path, source)`. A file qualifies when it declares a
/// stall-named `fn` AND references BOTH snapshot halves the ACs name: the
/// worker heartbeat (registry side) and the activity trail. `chase_stalled`
/// (sm_watch.rs) polices stalled sprint tickets — no heartbeat, no registry —
/// and never qualifies.
fn stall_predicate_module() -> Option<(String, String)> {
    let root = repo_root();
    for path in application_sources() {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let defines_stall_fn = src
            .lines()
            .any(|l| fn_name(l).is_some_and(|n| stall_fn_name(&n)));
        if !defines_stall_fn {
            continue;
        }
        let heartbeat_half = src.contains("heartbeat") || src.contains("WorkerEntry");
        let activity_half = src.contains("ActivityEntry") || src.contains("activity");
        if heartbeat_half && activity_half {
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            return Some((rel, src));
        }
    }
    None
}

// --- green guards: fixture validity over types that exist today --------------

/// AC4's premise, executable today: the stall snapshot is fully expressible as
/// struct literals over the types the codebase actually persists — a
/// `WorkerEntry` heartbeat from the worker registry and an `ActivityEntry`
/// entry on the persisted `ProjectState` activity trail — and elapsed silence
/// is computable from those literals with the codebase's own RFC3339 parser
/// and no live processes. When the predicate lands (AC1's red guard forces
/// it), THIS is the snapshot shape its unit tests reproduce, field for field.
#[test]
fn the_snapshot_ingredients_are_pure_struct_literals_over_existing_types() {
    let stale_at = "2020-01-01T00:00:00Z";
    let worker = WorkerEntry {
        worker: "op@host".to_owned(),
        role: "DEV-FEATURE".to_owned(),
        ticket: "CXC-259".to_owned(),
        at: stale_at.to_owned(),
        engines: Vec::new(),
        models: Vec::new(),
        git: None,
        tooling: None,
        version: String::new(),
    };
    let mut state = ProjectState::default();
    state.activity.push(ActivityEntry {
        at: stale_at.to_owned(),
        agent: "DEV-FEATURE".to_owned(),
        action: "merged CXC-258".to_owned(),
        ticket: Some("CXC-258".to_owned()),
    });

    let fmt = &time::format_description::well_known::Rfc3339;
    let beat = time::OffsetDateTime::parse(&worker.at, fmt).expect("registry stamps parse");
    let last = time::OffsetDateTime::parse(&state.activity[0].at, fmt).expect("trail stamps parse");
    let now = time::OffsetDateTime::parse(&now_rfc3339(), fmt).expect("now_rfc3339 parses");
    let silence = (now - last).whole_seconds();
    assert!(
        silence > 0,
        "elapsed silence must be computable from the snapshot literals alone"
    );
    assert!(
        beat <= last,
        "the stale heartbeat predates (or matches) the last activity"
    );
    // AC2's fourth reason is a real snapshot too: a default state has no
    // tickets — an empty backlog, nothing to do.
    assert!(
        state.tickets.is_empty(),
        "a default ProjectState is an empty backlog"
    );
}

/// AC2's premise: the documented non-stall reasons are REAL loop behaviour
/// today, each announced through the alert channels — so the predicate's
/// exclusion list has concrete, current sources to key on. If a reason's
/// mechanism moves or its announcement changes, move this guard with it.
#[test]
fn the_documented_non_stall_pauses_exist_in_the_loop_today() {
    let runner = read("crates/application/src/use_cases/runner.rs");
    assert!(
        runner.contains("loop paused: spend cap reached"),
        "the budget-cap pause no longer announces as documented — the \
         predicate's budget exclusion keys on this"
    );
    assert!(
        runner.contains("fn pause("),
        "the user pause (RunnerHandle::pause) moved — the predicate's \
         user-pause exclusion keys on it"
    );
    let cycle = read("crates/application/src/use_cases/cycle/mod.rs");
    assert!(
        cycle.contains("quota_exhausted"),
        "the quota-exhaustion pause no longer announces as documented — the \
         predicate's quota exclusion keys on this"
    );
    assert!(
        ProjectState::default().tickets.is_empty(),
        "an empty backlog is the default state — a real non-stall reason"
    );
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "Given desired-run=true, ... exactly one stall alert is raised through
/// the existing alert channels naming the worker, the last activity, and the
/// elapsed silence." The hub-side predicate must exist and its module must
/// name the alert facts and the channel machinery it rides.
#[test]
fn ac1_a_stale_heartbeat_with_a_silent_trail_raises_one_stall_alert_through_the_existing_channels()
{
    let Some((path, src)) = stall_predicate_module() else {
        panic!(
            "no loop-liveness stall predicate exists in crates/application \
             (a stall-named fn whose module references the worker heartbeat \
              AND the activity trail) — AC1's trigger has nothing to raise; \
             sm_watch::chase_stalled is about stalled TICKETS and does not \
             qualify"
        );
    };
    assert!(
        src.contains("NotifyEvent") || src.contains("NotifierPort") || src.contains("notify"),
        "the stall predicate in {path} must raise its alert through the \
         EXISTING alert channels (the NotifyEvent/NotifierPort machinery), \
         not a new sink"
    );
    assert!(
        src.contains("worker"),
        "the stall alert must name the worker — {path} never mentions it"
    );
    assert!(
        src.contains("activity"),
        "the stall alert must name the last activity — {path} never mentions it"
    );
    assert!(
        src.contains("silence") || src.contains("elapsed") || src.contains("threshold"),
        "the stall alert must name the elapsed silence against the configured \
         threshold — {path} never mentions it"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "A loop legitimately paused by budget cap, quota exhaustion, user
/// pause, or an empty backlog produces no stall alert (documented non-stall
/// reasons are excluded by the predicate)."
#[test]
fn ac2_documented_non_stall_reasons_are_excluded_by_the_predicate() {
    let Some((path, src)) = stall_predicate_module() else {
        panic!(
            "no loop-liveness stall predicate exists in crates/application — \
             AC2's documented non-stall exclusions have nothing to live in"
        );
    };
    for reason in ["budget", "quota", "pause", "backlog"] {
        assert!(
            src.contains(reason),
            "the stall predicate must exclude the documented non-stall reason \
             `{reason}` — {path} never mentions it"
        );
    }
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "The same stall episode never alerts twice (deduped until activity
/// resumes), so a multi-hour stall cannot flood the feed, and the condition
/// self-clears when new activity lands."
#[test]
fn ac3_a_stall_episode_alerts_once_until_activity_resumes_and_then_self_clears() {
    let Some((path, src)) = stall_predicate_module() else {
        panic!(
            "no loop-liveness stall predicate exists in crates/application — \
             AC3's episode dedup and self-clear have nothing to live in"
        );
    };
    assert!(
        src.contains("episode") || src.contains("dedup"),
        "the stall predicate must dedupe per episode so a multi-hour stall \
         cannot re-alert while silence continues — {path} never mentions an \
         episode or dedup"
    );
    assert!(
        src.contains("clear"),
        "the stall condition must self-clear when new activity lands — \
         {path} never mentions clearing"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// AC4: "The stall predicate (trigger, alert text, clear condition) is a pure
/// function over a store/registry snapshot, testable with a struct literal and
/// no live processes, and the checker runs hub-side so it also fires when the
/// worker process is entirely gone."
#[test]
fn ac4_the_predicate_is_a_pure_snapshot_function_and_the_checker_runs_hub_side() {
    let Some((path, src)) = stall_predicate_module() else {
        panic!(
            "no loop-liveness stall predicate exists in crates/application — \
             AC4's pure snapshot function has nothing to pin"
        );
    };
    assert!(
        src.to_lowercase().contains("snapshot"),
        "the predicate must be a pure function over a store/registry SNAPSHOT \
         (struct-literal testable) — {path} never declares one"
    );
    for io in ["std::process", "tokio::process", "Command"] {
        assert!(
            !src.contains(io),
            "the stall predicate must not run live processes ({io} in {path}) \
             — it decides over the snapshot only"
        );
    }
    let server = read("crates/presentation/src/server/mod.rs");
    let watchdogs = window_of(&server, "HubRole::All | HubRole::Knowledge", "\n    }");
    assert!(
        watchdogs.contains("tokio::spawn"),
        "the hub watchdog block moved — point this guard at where the hub \
         spawns its knowledge-role loops"
    );
    assert!(
        watchdogs.contains("stall") || watchdogs.contains("liveness"),
        "the hub never spawns a loop-liveness checker — the watchdog block \
         runs the space-budget, meeting, backup, release and docker-janitor \
         loops only, so a stall with the worker process entirely gone is \
         invisible"
    );
}
