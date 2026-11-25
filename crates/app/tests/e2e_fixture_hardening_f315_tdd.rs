//! CXA-F315 — Harden e2e fixtures against port squatters and the SSE race.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "Given a previous e2e run was interrupted and left its fixture server
//!    still bound to the suite's port (4517 or 4518), starting that suite
//!    again boots a fresh fixture server on the same port and runs to
//!    completion — no 'port already used' failure and no manual process
//!    killing, regardless of how long the stale server takes to release the
//!    port (up to a few seconds)."
//! 2. "After any e2e run — including one interrupted mid-suite — the next run
//!    of either suite on the same machine needs no manual cleanup: the
//!    unauthenticated suite never renders a login wall, and no leftover
//!    account/state residue from one suite changes the other suite's RBAC
//!    behaviour."
//! 3. "With retries disabled as configured, a spec that opens the app and
//!    immediately interacts with ticket data (e.g. opens a ticket dialog)
//!    succeeds on every run — 10 consecutive back-to-back runs of the specs
//!    directory produce zero SSE-related flakes, timeouts, or
//!    missing-ticket failures."
//! 4. "Golden screenshots are stable across consecutive runs: repeated runs
//!    of the same spec against the frozen fixture produce pixel diffs only
//!    within the configured threshold, with no intermittent mismatches
//!    caused by the UI rendering before or while the state snapshot arrives."
//! 5. "If the fixture server fails to deliver the state snapshot, the
//!    affected spec fails with a clear readiness/timeout failure naming the
//!    unhydrated app state, not an unrelated element-not-found error deep
//!    inside the spec's interactions."
//!
//! WHERE THE SUBJECTS LIVE (this tree): the sourced port/identity guard
//! (`e2e/fixture-guard.sh`), the two fixture boot scripts (`e2e/run-server.sh`,
//! `e2e/run-server-auth.sh`), the two Playwright configs, and the two
//! per-suite spec helpers (`e2e/specs/helpers.mjs`,
//! `e2e/specs-auth/helpers-auth.mjs`). The facts the predicates below lean
//! on, all read from the code today:
//! - `build_auth(state_dir.parent())` writes `auth.json` beside the state
//!   dir's PARENT (crates/app/src/lib.rs:619 →
//!   infrastructure/src/auth.rs `FileAuthService::default_path` =
//!   `base.join("auth.json")`), and sessions persist to
//!   `sessions.json` beside it (`with_file_name("sessions.json")`) — so the
//!   wipe that must make each run fresh is the wipe of the per-suite TREE,
//!   not of the nested `serve/` copy alone.
//! - The app hydrates `STATE` (the classic-script global in
//!   web/js/core.js) from the 1 Hz snapshot stream
//!   `/api/projects/:pid/events`, and a spec that touches ticket data before
//!   that snapshot lands races it — the exact hazard the suite's own
//!   `openApp` doc-comment describes.
//! - The real hub serves `/api/openapi.json` with the title
//!   "CoXAgent Hub API" (crates/presentation/src/server/openapi.rs), while
//!   Playwright's webServer.url accepts ANY 2xx on /api/health — so identity
//!   must be proven by the boot wrapper, not by Playwright's readiness poll.
//!
//! GUARD STYLE: repo-source scans with pure predicates, the established
//! convention of this tree's acceptance gates for surfaces that have no
//! executable Rust seam (`evidence_repro_routes_f248_tdd.rs`): the e2e
//! fixture harness IS such a surface. No server, no harness, no port — the
//! predicates are pure functions over script text and are unit-tested on
//! synthetic scripts both ways, so a passing/failing file assertion is
//! always explainable by the predicate alone.
//!
//! AC → test map:
//! - AC1: [`ac1_a_stale_fixture_server_on_the_suite_port_never_blocks_the_next_boot`]
//!   (bounded release wait, sourced guard), [`ac1_the_guard_kills_only_attributed_holders_and_fails_fast_on_foreign_ones`],
//!   [`ac1_both_suites_prove_the_server_identity_before_the_specs_talk_to_it`],
//!   [`ac1_the_suite_entry_points_evict_stale_fixtures_before_playwright_probes`],
//!   [+ four synthetic predicate tests]
//! - AC2: [`ac2_the_auth_suite_wipes_its_whole_state_tree_including_the_account_file`],
//!   [`ac2_the_open_suite_clears_the_legacy_account_and_session_residue`]
//!   [+ one synthetic predicate test]
//! - AC3: [`ac3_retries_stay_disabled_and_each_suite_boots_a_fresh_fixture_server`],
//!   [`ac3_every_spec_touching_ticket_data_routes_through_a_hydration_gate_in_its_own_suite_helper`],
//!   [`ac3_openapp_routes_through_the_shared_snapshot_wait_in_both_suites`],
//!   [`ac3_openapp_never_waits_on_a_barrier_the_persistent_sse_stream_can_starve`]
//! - AC4: [`ac4_the_golden_threshold_and_animation_freeze_stay_pinned_in_the_open_config`]
//!   (hydration-before-screenshot is AC3's gate test — the same predicate)
//! - AC5: [`ac5_a_missing_state_snapshot_fails_the_spec_naming_the_unhydrated_app_state`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

const GUARD: &str = "e2e/fixture-guard.sh";
const OPEN_SCRIPT: &str = "e2e/run-server.sh";
const AUTH_SCRIPT: &str = "e2e/run-server-auth.sh";
const PKG: &str = "e2e/package.json";
const OPEN_CONFIG: &str = "e2e/playwright.config.ts";
const AUTH_CONFIG: &str = "e2e/playwright.auth.config.ts";
const OPEN_HELPER: &str = "e2e/specs/helpers.mjs";
const AUTH_HELPER: &str = "e2e/specs-auth/helpers-auth.mjs";
const SPECS_DIR: &str = "e2e/specs";

// --- repo-source scan helpers -------------------------------------------------

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

/// The script with whole-line `#` comments dropped, so prose in a header can
/// never satisfy a code predicate. These scripts only comment whole lines.
fn shell_code(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One-level `$VAR`/`${VAR}` substitution from literal `NAME="..."`
/// assignments, leaving unknown variables (like the command-substituted
/// `$HERE`) untouched so targets stay comparable to their written form.
fn resolve(arg: &str, vars: &std::collections::BTreeMap<String, String>) -> String {
    let mut out = arg.trim().trim_matches('"').to_owned();
    for (name, value) in vars {
        for pat in [format!("${{{name}}}"), format!("${name}")] {
            if out.contains(&pat) {
                out = out.replace(&pat, value);
            }
        }
    }
    out
}

/// Every `rm` target with its flag kind, resolved against the script's
/// literal assignments. Pure over the script text. Only PLAIN literal values
/// are collected as variables: a command-substituted assignment such as
/// `HERE="$(cd …)"` would otherwise smear shell syntax into every `$HERE`
/// target and make written-form comparisons impossible.
fn rm_targets(code: &str) -> Vec<(String, String)> {
    let mut vars = std::collections::BTreeMap::new();
    for line in code.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim().trim_matches('"');
        let is_plain_literal = !value.contains("$(") && !value.contains('`');
        if name.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            && !name.is_empty()
            && is_plain_literal
        {
            vars.insert(name.to_owned(), value.to_owned());
        }
    }
    let mut out = Vec::new();
    for line in code.lines() {
        let Some(flag) = ["rm -rf", "rm -f"].iter().find(|f| line.contains(**f)) else {
            continue;
        };
        let args = line
            .split_whitespace()
            .skip(1)
            .filter(|t| !t.starts_with('-'));
        for arg in args {
            out.push(((*flag).to_string(), resolve(arg, &vars)));
        }
    }
    out
}

// --- AC1 predicates: attribution, bounded release wait, identity --------------

/// The port is ours by contract: after killing the squatter, the boot must
/// PROBE the port in a bounded loop until it is actually released — a single
/// fixed sleep fails the criterion the moment a stale server needs a few
/// seconds to exit. Pure over the script text.
fn release_wait(src: &str) -> Result<(), String> {
    let code = shell_code(src);
    let kill_at = code
        .find("lsof -ti")
        .ok_or("script never kills the port squatter at all")?;
    let after_kill = &code[kill_at + code[kill_at..].find('\n').unwrap_or(0)..];
    let boot_at = after_kill.find("exec").unwrap_or(after_kill.len());
    let window = &after_kill[..boot_at];
    let loops = window.contains("until") || window.contains("while");
    let probes = window.contains("lsof") || window.contains("nc -z");
    let bounded = [
        "for ", "seq ", "-le ", "-lt ", "-gt ", "i=0", "i=1", "attempts", "tries",
    ]
    .iter()
    .any(|m| window.contains(m));
    if !(loops && probes && bounded) {
        return Err(format!(
            "no bounded release wait between killing the squatter and exec: loop={loops} \
             port_probe={probes} bounded={bounded}. A fixed `sleep 1` fails as soon as the \
             stale fixture server needs more than that to release the port — poll until the \
             port answers nothing (lsof/nc -z), bounded, before booting."
        ));
    }
    Ok(())
}

/// Eviction is attribution-checked, never blind (the deploy_smoke.rs
/// decide_holder pattern): the holder's command is inspected, only a holder
/// that proves it is this repo's fixture server is killed, and any other
/// holder fails the boot naming pid and command with the port intact.
/// Pure over the guard text.
fn attributed_eviction(src: &str) -> Result<(), String> {
    let code = shell_code(src);
    let listed = code.contains("lsof -ti");
    // `-o command=` is the inspection marker: the holder's command line is
    // what attribution is decided from (flag spelling like -ww may vary).
    let inspected = code.contains("-o command=");
    let attributed = code.contains("coxagent");
    let fails_fast = code.contains("exit 1");
    if !(listed && inspected && attributed && fails_fast) {
        return Err(format!(
            "guard does not attribute before evicting: lists_holders={listed} \
             inspects_holder_command={inspected} attributed_to_fixture_binary={attributed} \
             foreign_fail_fast={fails_fast}. Kill only holders whose command proves they are \
             this repo's fixture server; any other holder must fail the boot in seconds \
             naming pid and command, port intact."
        ));
    }
    Ok(())
}

// --- AC2 predicate: the wipe covers the per-suite state TREE ------------------

/// `auth.json` and `sessions.json` land at the state dir's PARENT, so the
/// pre-boot wipe must remove the per-suite tree root itself. Wiping only the
/// nested `serve/` copy leaves the account file (and every live bearer
/// session in it) to outlive the run. Pure over the script text.
fn wipes_tree(src: &str, tree_root: &str) -> Result<(), String> {
    let code = shell_code(src);
    let wiped = rm_targets(&code).iter().any(|(flag, target)| {
        flag == "rm -rf" && target.trim_end_matches('/') == tree_root.trim_end_matches('/')
    });
    if !wiped {
        return Err(format!(
            "no `rm -rf {tree_root}` before boot — the run's account file \
             (auth.json) and its live sessions (sessions.json) live at the tree root, \
             so a wipe scoped to a nested directory leaves residue every later run inherits"
        ));
    }
    Ok(())
}

fn clears_legacy(src: &str, target: &str) -> bool {
    rm_targets(&shell_code(src))
        .iter()
        .any(|(_, t)| t.trim_end_matches('/') == target.trim_end_matches('/'))
}

// --- AC3/AC5 predicates: the hydration gate and its failure -------------------

/// The body of one helper's `openApp` — from its `export async function
/// openApp` to the next top-level `export`, so the predicate reads only what
/// that helper actually does.
fn open_app_body(src: &str) -> String {
    let Some(start) = src.find("export async function openApp") else {
        return String::new();
    };
    let rest = &src[start..];
    let end = rest[10..]
        .find("\nexport ")
        .map_or(rest.len(), |rel| 10 + rel);
    rest[..end].to_owned()
}

/// The body of the shared hydration gate — from its `export async function
/// awaitStateSnapshot` to the next top-level `export`.
fn await_state_snapshot_body(src: &str) -> String {
    let Some(start) = src.find("export async function awaitStateSnapshot") else {
        return String::new();
    };
    let rest = &src[start..];
    let end = rest[10..]
        .find("\nexport ")
        .map_or(rest.len(), |rel| 10 + rel);
    rest[..end].to_owned()
}

/// The gate that makes "open the app, immediately interact with ticket data"
/// safe: openApp must route through the shared `awaitStateSnapshot` wait
/// before handing control to the spec. The wait itself is pinned by
/// [`snapshot_wait_is_the_real_gate`]. Pure over the helper text.
fn routes_through_snapshot_wait(src: &str) -> Result<(), String> {
    let body = open_app_body(src);
    if body.is_empty() {
        return Err("helper has no openApp at all".to_owned());
    }
    if !body.contains("awaitStateSnapshot") {
        return Err(
            "openApp never waits for the hydrated app state — a spec that interacts \
             immediately races the 1 Hz snapshot stream"
                .to_owned(),
        );
    }
    Ok(())
}

/// The shared wait must be a real gate: after goto, it waits for the hydrated
/// app state (the classic-script `STATE` global carrying tickets). Pure over
/// the helper text.
fn snapshot_wait_is_the_real_gate(src: &str) -> Result<(), String> {
    let def = await_state_snapshot_body(src);
    if def.is_empty() {
        return Err(
            "no exported awaitStateSnapshot — the hydration gate is not defined".to_owned(),
        );
    }
    let waits = def.contains("waitForFunction") || def.contains("expect.poll");
    let on_state = def.contains("STATE") && def.contains("tickets");
    if !(waits && on_state) {
        return Err(
            "awaitStateSnapshot never waits for the hydrated app state (STATE with tickets) — \
             a spec that interacts immediately races the 1 Hz snapshot stream"
                .to_owned(),
        );
    }
    Ok(())
}

/// `networkidle` can NEVER fire while the snapshot stream holds its
/// connection open — the stream opens as soon as the app connects, so a run
/// where it wins the race turns every spec into a 30s timeout. The hydration
/// wait subsumes what networkidle was for, so openApp must not lean on it
/// (or, if kept, must bound and tolerate its failure). Pure over the text.
fn no_sse_starvable_barrier(body: &str) -> Result<(), String> {
    let Some(at) = body.find("waitForLoadState('networkidle')") else {
        return Ok(());
    };
    let call = &body[at..];
    let bounded = call.contains("timeout");
    let tolerated = body[at..].contains("catch");
    if bounded && tolerated {
        return Ok(());
    }
    Err(format!(
        "openApp waits on networkidle unguarded (bounded={bounded} tolerated={tolerated}) — \
         the persistent SSE connection can starve networkidle forever and fail every spec \
         with a timeout instead of the hydration wait"
    ))
}

/// AC5: when the snapshot never arrives, the failure must NAME the unhydrated
/// app state — an explicit thrown error, not Playwright's bare function
/// timeout, which reads as an unrelated spec bug. Pure over the text.
fn names_unhydrated_state(body: &str) -> Result<(), String> {
    let names_it =
        body.contains("new Error") && (body.contains("snapshot") || body.contains("hydrat"));
    if !names_it {
        return Err(
            "the readiness wait carries no failure naming the unhydrated app state — \
             when the fixture server fails to deliver the state snapshot, the spec dies on \
             a generic timeout instead of a readiness error"
                .to_owned(),
        );
    }
    Ok(())
}

// --- AC1 ----------------------------------------------------------------------

#[test]
fn ac1_a_stale_fixture_server_on_the_suite_port_never_blocks_the_next_boot() {
    // The bounded release wait lives in the sourced guard (shared by both
    // suites); each script must source it and run eviction + the wait.
    release_wait(&read(GUARD)).unwrap_or_else(|why| panic!("{GUARD}: {why}"));
    for script in [OPEN_SCRIPT, AUTH_SCRIPT] {
        let src = read(script);
        assert!(
            src.contains("fixture-guard.sh"),
            "{script} must source the shared port/identity guard"
        );
        assert!(
            src.contains("evict_stale_fixture"),
            "{script} never evicts the stale fixture holder on its port"
        );
        assert!(
            src.contains("await_port_free"),
            "{script} never waits for the port to actually release before booting"
        );
    }
}

#[test]
fn ac1_the_suite_entry_points_evict_stale_fixtures_before_playwright_probes() {
    // Verified live (2026-09-02): Playwright's webServer.url availability
    // probe runs BEFORE the webServer command, so a healthy stale fixture on
    // the suite port aborts `playwright test` with "already used" in
    // milliseconds — run-server.sh's own eviction never executes in that
    // case. The eviction therefore belongs in the entry point that launches
    // playwright, before the probe can see the stale holder.
    let pkg: serde_json::Value =
        serde_json::from_str(&read(PKG)).expect("e2e/package.json must parse");
    for key in ["test", "test:auth", "baseline"] {
        let script = pkg["scripts"][key]
            .as_str()
            .unwrap_or_else(|| panic!("e2e/package.json has no `{key}` script"));
        assert!(
            script.contains("clear_fixture_ports"),
            "`{key}` launches playwright without clearing the fixture ports first — \
             a healthy stale fixture on the suite port aborts the run at the \
             webServer.url probe before run-server.sh is ever invoked"
        );
    }
    // "the next run of EITHER suite needs no cleanup" only holds if the
    // entry-point guard clears BOTH suite ports, not just the one it happens
    // to be launching.
    let guard_code = shell_code(&read(GUARD));
    for port in ["4517", "4518"] {
        assert!(
            guard_code.contains(port),
            "{GUARD}'s clear_fixture_ports does not cover suite port {port}"
        );
    }
}

#[test]
fn ac1_the_release_wait_predicate_rejects_the_blind_fixed_sleep() {
    // The exact shape both scripts shipped before CXA-F315: kill, one blind
    // second, boot.
    let today = "PORT=4517\nlsof -ti :\"$PORT\" 2>/dev/null | xargs kill 2>/dev/null || true\n\
                 sleep 1\nexec env COXAGENT_PORT=\"$PORT\" \"$BIN\" serve\n";
    let why = release_wait(today).unwrap_err();
    assert!(why.contains("bounded release wait"), "unhelpful: {why}");
}

#[test]
fn ac1_the_release_wait_predicate_accepts_a_bounded_release_probe() {
    let fixed = "PORT=4517\n\
                 lsof -ti :\"$PORT\" 2>/dev/null | xargs kill 2>/dev/null || true\n\
                 i=0\n\
                 until ! lsof -ti :\"$PORT\" >/dev/null 2>&1; do\n  \
                 i=$((i + 1)); [ \"$i\" -gt 50 ] && break\n  sleep 0.2\n\
                 done\n\
                 exec env COXAGENT_PORT=\"$PORT\" \"$BIN\" serve\n";
    release_wait(fixed).unwrap_or_else(|why| panic!("bounded probe rejected: {why}"));
}

#[test]
fn ac1_the_guard_kills_only_attributed_holders_and_fails_fast_on_foreign_ones() {
    attributed_eviction(&read(GUARD)).unwrap_or_else(|why| panic!("{GUARD}: {why}"));
}

#[test]
fn ac1_the_guard_predicate_rejects_the_blind_kill_without_attribution() {
    // The exact shape both scripts shipped before CXA-F315.
    let blind = "lsof -ti :\"$PORT\" 2>/dev/null | xargs kill 2>/dev/null || true\n";
    attributed_eviction(blind).unwrap_err();
}

#[test]
fn ac1_the_guard_predicate_accepts_attribution_then_eviction() {
    let ok = "pids=$(lsof -ti :\"$PORT\")\n\
              for pid in $pids; do\n  \
              cmd=$(ps -p \"$pid\" -o command=)\n  \
              case \"$cmd\" in\n    \
              *coxagent*) kill -TERM \"$pid\" ;;\n    \
              *) echo foreign; exit 1 ;;\n  \
              esac\n\
              done\n";
    attributed_eviction(ok).unwrap_or_else(|why| panic!("rejected: {why}"));
}

#[test]
fn ac1_both_suites_prove_the_server_identity_before_the_specs_talk_to_it() {
    for script in [OPEN_SCRIPT, AUTH_SCRIPT] {
        let src = read(script);
        // The auth suite's probe is the login-flavored wrapper, which calls
        // await_identity itself — either marker means identity is proven.
        let proves = src.contains("await_identity") || src.contains("await_auth_identity");
        assert!(
            proves,
            "{script} never proves the port answers with THIS repo's hub API — \
             Playwright's webServer.url accepts any 2xx on /api/health, so an impostor \
             would run the whole suite against the wrong server"
        );
    }
    let auth = read(AUTH_SCRIPT);
    assert!(
        auth.contains("await_auth_identity"),
        "{AUTH_SCRIPT} never proves the throwaway account store by logging in with its \
         bootstrap admin — leftover shared auth state would flip the suite's RBAC behaviour"
    );
    let guard = read(GUARD);
    assert!(
        guard.contains("CoXAgent Hub API"),
        "{GUARD} does not pin the identity marker the real hub serves at /api/openapi.json"
    );
}

// --- AC2 ----------------------------------------------------------------------

#[test]
fn ac2_the_auth_suite_wipes_its_whole_state_tree_including_the_account_file() {
    wipes_tree(&read(AUTH_SCRIPT), "$HERE/.state-auth")
        .unwrap_or_else(|why| panic!("{AUTH_SCRIPT}: {why}"));
}

#[test]
fn ac2_the_open_suite_clears_the_legacy_account_and_session_residue() {
    let src = read(OPEN_SCRIPT);
    // The nested wipe makes each run fresh; the two rm -f lines clear the
    // residue the OLD flat layout left in the suite directory itself —
    // auth.json is cleared today, its sessions sibling is not.
    assert!(
        clears_legacy(&src, "$HERE/auth.json"),
        "{OPEN_SCRIPT} stopped clearing the legacy flat-layout auth.json"
    );
    assert!(
        clears_legacy(&src, "$HERE/sessions.json"),
        "{OPEN_SCRIPT} clears the legacy auth.json but not the sessions.json the \
         auth store writes beside it — stale live bearer sessions survive in the \
         suite directory across runs"
    );
}

#[test]
fn ac2_the_wipe_predicate_tells_the_tree_root_from_a_nested_directory() {
    let only_serve = "STATE=\"$HERE/.state-auth/serve\"\nrm -rf \"$STATE\"\n";
    wipes_tree(only_serve, "$HERE/.state-auth").unwrap_err();
    let whole_tree = "STATE=\"$HERE/.state-auth/serve\"\nrm -rf \"$HERE/.state-auth\"\n";
    wipes_tree(whole_tree, "$HERE/.state-auth")
        .unwrap_or_else(|why| panic!("whole-tree wipe rejected: {why}"));
}

// --- AC3 ----------------------------------------------------------------------

#[test]
fn ac3_retries_stay_disabled_and_each_suite_boots_a_fresh_fixture_server() {
    // The criterion's stated precondition: "with retries disabled as
    // configured" — a retry would mask the race instead of fixing it.
    for config in [OPEN_CONFIG, AUTH_CONFIG] {
        let src = read(config);
        assert!(
            src.contains("retries: 0"),
            "{config} enabled retries — the SSE race must be fixed, not papered over"
        );
        assert!(
            src.contains("reuseExistingServer: false"),
            "{config} reuses an existing server — the fresh-boot contract (AC1) needs a real boot"
        );
    }
}

#[test]
fn ac3_every_spec_touching_ticket_data_routes_through_a_hydration_gate_in_its_own_suite_helper() {
    // (a) The gate itself, in BOTH helpers: both suites' apps hydrate over
    // the same 1 Hz snapshot stream, and both suites' specs call openApp.
    for helper in [OPEN_HELPER, AUTH_HELPER] {
        routes_through_snapshot_wait(&read(helper)).unwrap_or_else(|why| panic!("{helper}: {why}"));
    }
    // The shared wait is defined ONCE (open suite) and imported by the auth
    // suite — two private copies would drift.
    assert!(
        read(AUTH_HELPER).contains("../specs/helpers.mjs"),
        "{AUTH_HELPER} re-implements the snapshot wait instead of importing the shared one"
    );
    snapshot_wait_is_the_real_gate(&read(OPEN_HELPER))
        .unwrap_or_else(|why| panic!("{OPEN_HELPER}: {why}"));
    // (b) The routing: every open-suite spec that interacts with ticket data
    // or takes a golden screenshot goes through openApp, never a bare goto.
    let dir = repo_root().join(SPECS_DIR);
    let mut specs: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.ends_with(".spec.ts").then_some(name)
        })
        .collect();
    specs.sort();
    assert!(!specs.is_empty(), "no specs under {SPECS_DIR}");
    for name in specs {
        let src = read(&format!("{SPECS_DIR}/{name}"));
        let interacts = src.contains("showTicket")
            || src.contains("openNewTicket")
            || src.contains("toHaveScreenshot");
        if interacts {
            assert!(
                src.contains("openApp"),
                "{SPECS_DIR}/{name} touches ticket data but never calls openApp — it \
                 races the state snapshot the gate waits for"
            );
        }
    }
}

#[test]
fn ac3_the_snapshot_wait_predicate_tells_a_real_gate_from_a_stub() {
    let stub = "export async function awaitStateSnapshot(page) {\n  \
                await page.waitForTimeout(100);\n\
                }\n";
    snapshot_wait_is_the_real_gate(stub).unwrap_err();
    let real = "export async function awaitStateSnapshot(page) {\n  \
                await page.waitForFunction(() => typeof STATE !== 'undefined' && \
                STATE.tickets.length > 0, null, { timeout: 15000 });\n\
                }\n";
    snapshot_wait_is_the_real_gate(real).unwrap_or_else(|why| panic!("real gate rejected: {why}"));
}

#[test]
fn ac3_openapp_never_waits_on_a_barrier_the_persistent_sse_stream_can_starve() {
    for helper in [OPEN_HELPER, AUTH_HELPER] {
        let body = open_app_body(&read(helper));
        no_sse_starvable_barrier(&body).unwrap_or_else(|why| panic!("{helper}: {why}"));
    }
}

// --- AC4 ----------------------------------------------------------------------

#[test]
fn ac4_the_golden_threshold_and_animation_freeze_stay_pinned_in_the_open_config() {
    // The frozen fixture still ages (relative timestamps), so the threshold
    // is part of the stability contract; the animation freeze is what keeps
    // mid-render paints out of the pixels.
    let src = read(OPEN_CONFIG);
    assert!(
        src.contains("maxDiffPixelRatio"),
        "{OPEN_CONFIG} lost the screenshot diff threshold"
    );
    assert!(
        src.contains("animations: 'disabled'"),
        "{OPEN_CONFIG} lost the animation freeze — mid-transition paints become golden flakes"
    );
}

// --- AC5 ----------------------------------------------------------------------

#[test]
fn ac5_a_missing_state_snapshot_fails_the_spec_naming_the_unhydrated_app_state() {
    // The failure naming the unhydrated state lives in the shared wait both
    // suites' openApps route through.
    let def = await_state_snapshot_body(&read(OPEN_HELPER));
    names_unhydrated_state(&def).unwrap_or_else(|why| panic!("{OPEN_HELPER}: {why}"));
}
