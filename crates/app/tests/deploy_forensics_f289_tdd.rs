//! CXA-F289 — Deploy failure forensics bundle attached to the deploy/rollback
//! record. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "A failed deploy persists a size-capped bundle (stderr tail plus recent
//!    per-container logs) on that deploy attempt's record in project state,
//!    not just a one-line summary."
//! 2. "The bundle is visible on the deploy history / verify surface together
//!    with the rollback outcome, and can be copied or downloaded in full."
//! 3. "When compose failed before any container existed, the record explicitly
//!    states that no container logs are available instead of showing an empty
//!    log section."
//! 4. "Secret-shaped strings inside the bundle are masked before persistence
//!    and before rendering."
//! 5. "E2E: a deliberately failing deploy (bad compose service) shows the
//!    bundle with the real compose error text in the browser."
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the real state types the
//! codebase has today (`ProjectState`, `DeployStatus`, `RollbackStatus` —
//! serde round-trips over JSON documents, no engine, no harness, no network
//! port) plus source-scan guards over the files the hub actually builds and
//! serves (`state/ops.rs`, `use_cases/cycle/ops.rs`,
//! `infrastructure/src/deploy/docker_compose.rs`, served `web/js/*.js`) — the
//! same no-harness discipline as `live_repro_url_f246_tdd.rs` and
//! `evidence_repro_routes_f248_tdd.rs`. A test that called
//! `state.deploy.failure_bundle` directly could not compile today (the symbol
//! does not exist), so the red half pins the missing symbols where they must
//! be declared, and the green half pins executable semantics over the types
//! that DO exist. Every failing assertion below fails only because CXA-F289's
//! behaviour is missing; if an assertion's mechanism moves during
//! implementation, move the guard with it (the `preflight_f239_tdd.rs`
//! convention).
//!
//! Red today, and why:
//!   * AC1 — `DeployStatus` (state/ops.rs) carries only `at/ok/summary/
//!     commit_sha/health_check`: the one-line summary and nothing else. No
//!     failure-bundle field, no `DeployFailureBundle` type, no size cap, and
//!     the compose adapter (`docker_compose.rs::deploy`) reduces a failed
//!     `up` to its last non-empty stderr line — the stderr tail and the
//!     per-container logs are dropped on the floor.
//!   * AC2 — no served script references the bundle (it does not exist), and
//!     `last_rollback` is rendered NOWHERE in the web UI (grep proves it):
//!     rollback outcomes are invisible today, let alone next to the deploy
//!     record.
//!   * AC3 — no record field and no render branch distinguish "compose failed
//!     before any container existed" from "logs were collected": an empty log
//!     section is the only possible render.
//!   * AC4 — nothing on the capture→persist path (adapter, `record_deploy`,
//!     state types) masks secret-shaped strings, and the render side cannot
//!     mask what does not exist.
//!
//! Green guards (fixture validity — house rule: every fixture must be
//! buildable from data the codebase actually has):
//!   * the deploy attempt record the bundle attaches to is REAL and
//!     constructible today (`DeployStatus`, `RollbackStatus` struct literals);
//!   * a legacy `DeployStatus` document without a bundle key loads and
//!     serializes unchanged — the additive-field convention the new field must
//!     follow (`#[serde(default, skip_serializing_if = ...)]`), pinned
//!     behaviourally so pre-change snapshots never break;
//!   * the committed frozen e2e fixture (`e2e/fixtures/state/state.json`,
//!     `"deploy": null`) loads cleanly — the exact legacy document shape AC1
//!     governs.
//!
//! AC5 (the E2E criterion) is exercised by `e2e/specs/deploy-failure-bundle.spec.ts`
//! (written in this ticket, red at RUNTIME until the behaviour lands — it
//! seeds the bundle through the same store-RPC surface the runners use and
//! asserts on the real rendered page). The Rust side only gates that spec's
//! CONTRACT: it must seed the bundle + rollback outcome, use the real compose
//! error text, arm the shared console gate and take a golden screenshot —
//! the `e2e_acceptance_gate.rs` / `evidence_repro_routes_f248_tdd.rs`
//! anti-shrink pattern, so the spec cannot be parked or silently weakened.
//!
//! IMPLEMENTER NOTES (pinned contract — flag to SA before renaming):
//!   * The wire/state names pinned here: `DeployStatus.failure_bundle`
//!     (Option, serde-defaulted), `stderr_tail`, `container_logs`
//!     (per-container `{service, tail}`), `no_container_logs` (the explicit
//!     AC3 marker), and a `pub const *BUNDLE*/*FORENSICS*: usize` size cap in
//!     the state module (beside `MAX_INCIDENTS`/`MAX_REVERT_EVENTS` — the
//!     house cap convention). The intermediate carrier from the adapter to
//!     `record_deploy` is deliberately NOT pinned: extend `DeployReport` or
//!     add a port method, the state contract is what the ACs govern.
//!   * The masking helper's name is not pinned — the scan accepts
//!     mask/redact/scrub markers on the capture→persist path (adapter window,
//!     `record_deploy` window, or the state module) and in the bundle's
//!     render window. If the mechanism lands elsewhere, move the guard
//!     openly rather than loosening it silently.
//!   * AC2 says "deploy history / verify surface": today the deploy attempt's
//!     record surfaces on the Overview deploy panel (core.js `ov-deploy`) and
//!     the bundle rides THAT record, so the tests pin "some served script
//!     renders the bundle AND the rollback outcome with a copy/download
//!     affordance"; the Overview route is pinned end-to-end by the e2e spec.
//!     A fuller history view is the SA's call and out of this ticket's red.
//!
//! AC → test map:
//! - AC1: [`a_deploy_status_json_carrying_the_failure_bundle_round_trips_it`]
//!   (RED), [`the_failure_bundle_carries_both_the_stderr_tail_and_per_container_logs`]
//!   (RED), [`the_bundle_size_cap_is_a_named_const_within_a_forensics_sane_band`]
//!   (RED), [`the_deploy_attempt_record_declares_the_failure_bundle_field`]
//!   (RED), [`the_failure_bundle_field_names_are_declared_in_the_state_module`]
//!   (RED), [`the_compose_adapter_captures_the_stderr_tail_and_container_logs`]
//!   (RED), [`record_deploy_wires_the_bundle_onto_the_persisted_attempt_record`]
//!   (RED), plus the green [`the_deploy_attempt_and_rollback_records_exist_as_fixtures`],
//!   [`a_legacy_deploy_status_without_a_bundle_key_loads_unchanged`] and
//!   [`the_frozen_e2e_fixture_state_loads_cleanly_today`]
//! - AC2: [`a_served_script_renders_the_bundle_alongside_the_rollback_outcome`]
//!   (RED), [`the_bundle_surface_offers_copy_or_download_of_the_full_text`]
//!   (RED), plus the executable browser half in the e2e spec (red at runtime)
//! - AC3: [`the_no_container_logs_marker_round_trips_as_an_explicit_record_field`]
//!   (RED), [`a_no_container_failure_sets_the_explicit_marker_and_the_render_names_it`]
//!   (RED)
//! - AC4: [`the_bundle_is_masked_before_persistence_and_again_before_rendering`]
//!   (RED), plus the executable render half in the e2e spec (red at runtime)
//! - AC5: [`the_e2e_spec_seeds_the_failing_deploy_and_gates_the_browser_contract`]
//!   (green gate over the spec's source; the spec itself is red at runtime)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::state::{DeployStatus, ProjectState, RollbackStatus};

/// The state field AC1 pins the bundle onto — "that deploy attempt's record in
/// project state" is `state.deploy` (`DeployStatus`).
const FIELD: &str = "failure_bundle";

/// AC3's explicit marker — a record-level statement, never an empty section.
const NO_LOGS_FIELD: &str = "no_container_logs";

/// The compose adapter whose failed `up` must yield the bundle's raw material.
const ADAPTER: &str = "crates/infrastructure/src/deploy/docker_compose.rs";

/// The cycle's deploy-recording path — where the bundle must reach the
/// persisted `DeployStatus`.
const RECORD_DEPLOY: &str = "crates/application/src/use_cases/cycle/ops.rs";

/// The state module files the bundle type and its size cap belong beside.
const STATE_OPS: &str = "crates/application/src/state/ops.rs";
const STATE_MOD: &str = "crates/application/src/state/mod.rs";

/// The committed pre-change persisted snapshot: the frozen e2e fixture is a
/// real `ProjectState` document whose `"deploy"` is null — the legacy shape
/// the additive field must never break.
const LEGACY_SNAPSHOT: &str = "e2e/fixtures/state/state.json";

/// The AC5 browser spec written with this ticket (red at runtime until the
/// behaviour lands; gated here so it cannot be parked or weakened).
const E2E_SPEC: &str = "e2e/specs/deploy-failure-bundle.spec.ts";

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

/// The source window of one top-level item: from its `header` to the next item
/// introduced by `terminator` (or end of file). Everything the item declares
/// lives inside this window. An absent header yields an empty window — the
/// caller's own assertion, not an index panic, must report the miss.
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

/// The source window of the `DeployStatus` struct — the deploy attempt's
/// record in project state. Every field it declares lives inside it.
fn deploy_status_window(src: &str) -> &str {
    window_of(src, "pub struct DeployStatus", "\n}")
}

/// The source window of `record_deploy` — the path a deploy outcome takes into
/// persisted state. Terminates at the next method's doc comment.
fn record_deploy_window(src: &str) -> &str {
    window_of(
        src,
        "async fn record_deploy",
        "\n    /// Mandatory post-deploy",
    )
}

/// The source window of the compose adapter's `deploy` — from its header to
/// the closing brace at column zero (the end of its impl block). Everything
/// the failed-`up` path does lives inside it.
fn adapter_deploy_window(src: &str) -> &str {
    window_of(src, "async fn deploy(&self, work_dir: &Path)", "\n}")
}

/// The dashboard view code: classic scripts sharing one scope, served by the
/// hub (see AGENTS.md — any UI change must pass the e2e gate).
fn web_js_sources() -> Vec<(String, String)> {
    let dir = repo_root().join("crates/presentation/src/web/js");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("list web/js")
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension()?.to_str()? == "js" {
                let name = p.file_name()?.to_str()?.to_owned();
                let src = std::fs::read_to_string(&p).ok()?;
                Some((name, src))
            } else {
                None
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The script(s) that render the failure bundle — pinned by the state field's
/// own name, since no view identifier exists for it yet.
fn bundle_scripts() -> Vec<(String, String)> {
    web_js_sources()
        .into_iter()
        .filter(|(_, src)| src.contains(FIELD))
        .collect()
}

/// Whether a bundle render window offers copying or downloading the FULL text
/// (the clipboard API or a download affordance) — AC2's "in full".
fn offers_copy_or_download(js: &str) -> bool {
    let lower = js.to_lowercase();
    lower.contains("clipboard") || lower.contains("download")
}

/// Whether a text mentions masking under any of the house's names for it.
fn mentions_masking(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("mask") || lower.contains("redact") || lower.contains("scrub")
}

/// Extract `pub const <NAME>: usize = <N>;` for every forensics-bundle cap in
/// the state sources, so the cap's magnitude is pinned without hard-coding the
/// const's exact name. Returns (name, value) pairs.
fn bundle_cap_consts(state_sources: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for line in state_sources.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("pub const ") else {
            continue;
        };
        let Some((name, value_part)) = rest.split_once(':') else {
            continue;
        };
        let upper = name.to_uppercase();
        if !upper.contains("BUNDLE") && !upper.contains("FORENSICS") {
            continue;
        }
        let Some(equals) = value_part.split_once('=') else {
            continue;
        };
        let digits: String = equals.1.chars().filter(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<usize>() {
            out.push((name.trim().to_owned(), n));
        }
    }
    out
}

// --- green guards (fixture validity — pass today, must stay green) -----------

/// AC1 fixture validity: the record the bundle attaches to, and the rollback
/// outcome record AC2 pairs it with, are real, constructible types — the
/// bundle is an additive field on THESE, not a new surface invented from
/// nothing.
#[test]
fn the_deploy_attempt_and_rollback_records_exist_as_fixtures() {
    let attempt = DeployStatus {
        at: "2026-09-01T00:00:00Z".to_owned(),
        ok: false,
        summary: "docker compose failed: exit 1".to_owned(),
        commit_sha: None,
        health_check: None,
        failure_bundle: None,
    };
    assert!(!attempt.ok, "fixture: a failed deploy attempt");
    let rollback = RollbackStatus {
        at: "2026-09-01T00:00:01Z".to_owned(),
        reason: "deploy failed".to_owned(),
        to_sha: "abc1234def".to_owned(),
        ok: true,
        summary: "rolled back to abc1234".to_owned(),
        stale: false,
        migration_blocked: false,
        failure_bundle: None,
    };
    assert!(rollback.ok, "fixture: a rollback outcome to show beside it");
}

/// AC1 (additive-field convention): a legacy `DeployStatus` document — written
/// before the bundle existed — must load unchanged and serialize without the
/// new key, so pre-change persisted state never fails a load. Behavioural pin;
/// the source-scan twin pins the `#[serde(default)]` declaration.
#[test]
fn a_legacy_deploy_status_without_a_bundle_key_loads_unchanged() {
    let legacy = r#"{
        "at": "2026-09-01T00:00:00Z",
        "ok": false,
        "summary": "docker compose failed: exit 18"
    }"#;
    let status: DeployStatus = serde_json::from_str(legacy).expect("legacy record loads");
    assert!(!status.ok);
    let out = serde_json::to_string(&status).expect("serialize");
    assert!(
        !out.contains(FIELD),
        "a legacy record must not grow an invented {FIELD} key on round-trip: {out}"
    );
}

/// AC1 (legacy state): the committed frozen fixture — a real `ProjectState`
/// document with `"deploy": null` — loads cleanly today and must still load
/// after the bundle field lands.
#[test]
fn the_frozen_e2e_fixture_state_loads_cleanly_today() {
    let raw = read(LEGACY_SNAPSHOT);
    let state: ProjectState = serde_json::from_str(&raw).expect("frozen fixture loads");
    assert!(
        state.deploy.is_none(),
        "fixture shape: no deploy attempt yet"
    );
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "A failed deploy persists a size-capped bundle (stderr tail plus
/// recent per-container logs) on that deploy attempt's record in project
/// state, not just a one-line summary." Persistence half, executable over the
/// wire contract: a `DeployStatus` document carrying the bundle must
/// round-trip through the state type with the bundle intact — today the
/// unknown field is silently DROPPED by serde, which is exactly the
/// one-line-summary-only behaviour the AC forbids.
#[test]
fn a_deploy_status_json_carrying_the_failure_bundle_round_trips_it() {
    let carried = r#"{
        "at": "2026-09-01T00:00:00Z",
        "ok": false,
        "summary": "docker compose failed: exit 1",
        "failure_bundle": {
            "stderr_tail": "validating docker-compose.yml: service \"web\" has neither an image nor a build context specified",
            "container_logs": [],
            "no_container_logs": true
        }
    }"#;
    let status: DeployStatus = serde_json::from_str(carried).expect("the record loads");
    let out = serde_json::to_string(&status).expect("serialize");
    assert!(
        out.contains(FIELD),
        "the bundle must PERSIST on the attempt's record, not be dropped as an \
         unknown field: {out}"
    );
    assert!(
        out.contains("stderr_tail"),
        "the persisted bundle carries the stderr tail: {out}"
    );
}

/// AC1 (bundle shape): the persisted bundle carries BOTH halves the AC names —
/// the stderr tail AND the recent per-container logs, each log attributed to
/// its service. Round-trips a bundle with one container log and asserts both
/// survive; red today (the whole object is dropped).
#[test]
fn the_failure_bundle_carries_both_the_stderr_tail_and_per_container_logs() {
    let carried = r#"{
        "at": "2026-09-01T00:00:00Z",
        "ok": false,
        "summary": "docker compose failed: exit 1",
        "failure_bundle": {
            "stderr_tail": "docker compose up failed: exit 1",
            "container_logs": [
                { "service": "db", "tail": "pg_ctl: could not start server" }
            ],
            "no_container_logs": false
        }
    }"#;
    let status: DeployStatus = serde_json::from_str(carried).expect("the record loads");
    let out = serde_json::to_string(&status).expect("serialize");
    assert!(
        out.contains("stderr_tail") && out.contains("container_logs"),
        "both the stderr tail and the per-container logs must survive \
         persistence: {out}"
    );
    assert!(
        out.contains("\"service\":\"db\"") || out.contains("\"service\": \"db\""),
        "each container log is attributed to its service: {out}"
    );
}

/// AC1 (size-capped): the cap is a NAMED constant in the state module — beside
/// `MAX_INCIDENTS`/`MAX_REVERT_EVENTS`, the house convention for persisted
/// bounds — and its magnitude stays sane: at least 1 KiB (a tail that cannot
/// hold a compose error is no forensics) and at most 256 KiB (the state is
/// broadcast to every dashboard once a second; a bigger bundle would bloat
/// every poll). Red today: no such constant exists anywhere in the state
/// module.
#[test]
fn the_bundle_size_cap_is_a_named_const_within_a_forensics_sane_band() {
    let sources = read(STATE_OPS) + "\n" + &read(STATE_MOD);
    let caps = bundle_cap_consts(&sources);
    assert!(
        !caps.is_empty(),
        "the bundle's size cap must be a named `pub const …: usize` in the \
         state module (name containing BUNDLE or FORENSICS) — none found"
    );
    for (name, n) in &caps {
        assert!(
            (1_024..=262_144).contains(n),
            "{name} = {n} is outside the sane forensics band [1024, 262144]"
        );
    }
}

/// AC1 (declaration): the attempt record declares the bundle field with the
/// additive serde convention, so legacy snapshots default and the 1 Hz
/// snapshot does not carry the key when there is no bundle.
#[test]
fn the_deploy_attempt_record_declares_the_failure_bundle_field() {
    let src = read(STATE_OPS);
    let window = deploy_status_window(&src);
    assert!(
        window.contains(FIELD),
        "DeployStatus must declare the `{FIELD}` field — the deploy attempt's \
         record is where the bundle persists; struct window:\n{window}"
    );
    assert!(
        window.contains("serde(default"),
        "`{FIELD}` must be `#[serde(default, …)]` so pre-change snapshots \
         load untouched; struct window:\n{window}"
    );
}

/// AC1 (declaration, bundle type): the field names the wire contract pins —
/// `stderr_tail`, `container_logs`, `no_container_logs` — are declared in the
/// state module beside the record they ride on.
#[test]
fn the_failure_bundle_field_names_are_declared_in_the_state_module() {
    let src = read(STATE_OPS);
    for field in ["stderr_tail", "container_logs", NO_LOGS_FIELD] {
        assert!(
            src.contains(field),
            "the state module must declare `{field}` — the bundle's persisted \
             shape is the wire contract the dashboard renders"
        );
    }
}

/// AC1 (capture): the compose adapter's failed `up` must collect the raw
/// material — the stderr tail it already prints a one-line slice of, plus the
/// recent per-container logs (`docker compose logs`) — instead of reducing the
/// failure to its last non-empty stderr line. Red today: the failed-`up` path
/// neither collects logs nor sets the no-container-logs marker.
#[test]
fn the_compose_adapter_captures_the_stderr_tail_and_container_logs() {
    let src = read(ADAPTER);
    let window = adapter_deploy_window(&src);
    assert!(
        window.contains("\"logs\"") || window.contains("logs("),
        "the failed-`up` path must collect recent per-container logs \
         (`docker compose logs …`); adapter deploy window:\n{window}"
    );
    assert!(
        window.contains(NO_LOGS_FIELD),
        "the adapter must record the AC3 `no_container_logs` marker when \
         compose failed before any container existed; adapter deploy \
         window:\n{window}"
    );
}

/// AC1 (wiring): `record_deploy` — the one path a deploy outcome takes into
/// persisted state — must write the bundle onto the attempt's record. Red
/// today: the window wires only `at/ok/summary/commit_sha/health_check`.
#[test]
fn record_deploy_wires_the_bundle_onto_the_persisted_attempt_record() {
    let src = read(RECORD_DEPLOY);
    let window = record_deploy_window(&src);
    assert!(
        window.contains(FIELD),
        "record_deploy must persist the bundle onto the attempt's record \
         (`{FIELD}`); record_deploy window:\n{window}"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "The bundle is visible on the deploy history / verify surface together
/// with the rollback outcome." Source half: some SERVED dashboard script
/// renders the bundle, and that same surface renders the rollback outcome —
/// today no script references either beside the deploy summary, and
/// `last_rollback` is rendered nowhere at all. The executable browser half is
/// the e2e spec's job (Overview route).
#[test]
fn a_served_script_renders_the_bundle_alongside_the_rollback_outcome() {
    let scripts = bundle_scripts();
    assert!(
        !scripts.is_empty(),
        "no served web/js script references `{FIELD}` — the bundle is not \
         rendered anywhere"
    );
    for (name, src) in &scripts {
        assert!(
            src.contains("last_rollback"),
            "{name} renders the bundle but not the rollback outcome — AC2 \
             requires them visible together"
        );
    }
}

/// AC2: "…and can be copied or downloaded in full." The bundle's render window
/// must offer a copy-or-download affordance carrying the full bundle text —
/// a truncated on-screen preview does not satisfy the AC. Red today: the
/// bundle is not rendered, so no affordance exists.
#[test]
fn the_bundle_surface_offers_copy_or_download_of_the_full_text() {
    let scripts = bundle_scripts();
    assert!(
        !scripts.is_empty(),
        "no served script renders `{FIELD}` — nothing to copy or download"
    );
    for (name, src) in &scripts {
        assert!(
            offers_copy_or_download(src),
            "{name} renders the bundle without a copy/download affordance — \
             AC2 requires the full text to be copyable or downloadable"
        );
    }
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "When compose failed before any container existed, the record
/// explicitly states that no container logs are available instead of showing
/// an empty log section." Persistence half, executable: the explicit marker
/// (not an empty-list guess) must round-trip as a real record field. Red
/// today: the whole bundle object is dropped as an unknown field.
#[test]
fn the_no_container_logs_marker_round_trips_as_an_explicit_record_field() {
    let carried = format!(
        r#"{{
        "at": "2026-09-01T00:00:00Z",
        "ok": false,
        "summary": "docker compose failed: exit 1",
        "{FIELD}": {{
            "stderr_tail": "validating docker-compose.yml: services.web must be a mapping",
            "container_logs": [],
            "{NO_LOGS_FIELD}": true
        }}
    }}"#
    );
    let status: DeployStatus = serde_json::from_str(&carried).expect("the record loads");
    let out = serde_json::to_string(&status).expect("serialize");
    assert!(
        out.contains(NO_LOGS_FIELD),
        "the explicit no-container-logs marker must persist as a record \
         field, not collapse into an empty log list: {out}"
    );
}

/// AC3 (render half + capture half): the adapter's no-container path must SET
/// the explicit marker rather than hand back an empty log list, and the
/// bundle's render window must branch on it to show the explicit statement —
/// an empty `<pre></pre>` section is exactly what the AC forbids. Red today:
/// neither side exists.
#[test]
fn a_no_container_failure_sets_the_explicit_marker_and_the_render_names_it() {
    let adapter = read(ADAPTER);
    assert!(
        adapter_deploy_window(&adapter).contains(NO_LOGS_FIELD),
        "the adapter must set `{NO_LOGS_FIELD}` when compose failed before \
         any container existed — the record states it, it is not derived by \
         guessing from an empty list"
    );
    let scripts = bundle_scripts();
    assert!(
        !scripts.is_empty(),
        "no served script renders `{FIELD}` — the explicit no-logs statement \
         cannot render"
    );
    for (name, src) in &scripts {
        assert!(
            src.contains(NO_LOGS_FIELD),
            "{name} must branch on `{NO_LOGS_FIELD}` to render the explicit \
             'no container logs available' statement instead of an empty log \
             section"
        );
    }
}

// --- AC4 ---------------------------------------------------------------------

/// AC4: "Secret-shaped strings inside the bundle are masked before persistence
/// and before rendering." Persistence half: somewhere on the capture→persist
/// path (the adapter's failed-`up` window, `record_deploy`, or the state
/// module that builds the bundle) a masking step must run BEFORE the bundle
/// reaches `store.save` — a leaked credential must never exist in the
/// persisted state file in the first place. Render half: the bundle's render
/// window must mask again (defence in depth — state may already carry a
/// legacy unmasked bundle). Red today: no masking exists on either side; the
/// executable browser half (a seeded secret must never appear in the page) is
/// the e2e spec's job.
#[test]
fn the_bundle_is_masked_before_persistence_and_again_before_rendering() {
    let adapter = read(ADAPTER);
    let record = read(RECORD_DEPLOY);
    let state = read(STATE_OPS);
    let persist_path = format!(
        "{}\n{}\n{}",
        adapter_deploy_window(&adapter),
        record_deploy_window(&record),
        state
    );
    assert!(
        mentions_masking(&persist_path),
        "the capture→persist path must mask secret-shaped strings BEFORE the \
         bundle is persisted (adapter deploy window, record_deploy, or the \
         state module that builds the bundle)"
    );
    let scripts = bundle_scripts();
    assert!(
        !scripts.is_empty(),
        "no served script renders `{FIELD}` — the render-side mask cannot exist"
    );
    for (name, src) in &scripts {
        assert!(
            mentions_masking(src),
            "{name} renders the bundle without masking — legacy unmasked \
             bundles in persisted state must not leak through the render"
        );
    }
}

// --- AC5 ---------------------------------------------------------------------

/// AC5: "E2E: a deliberately failing deploy (bad compose service) shows the
/// bundle with the real compose error text in the browser." The browser half
/// runs in `e2e/specs/deploy-failure-bundle.spec.ts` (red at runtime until the
/// behaviour lands). This gate pins that spec's contract so it can neither be
/// parked nor silently weakened (the `e2e_acceptance_gate.rs` pattern): it
/// must seed the bundle + rollback outcome through the runners' own store-RPC
/// surface, use the REAL compose error text for a bad service, cover the
/// no-container-logs statement, arm the shared console gate, and take a
/// golden screenshot.
#[test]
fn the_e2e_spec_seeds_the_failing_deploy_and_gates_the_browser_contract() {
    let spec = read(E2E_SPEC);
    // A deliberately failing deploy: the real text `docker compose` prints for
    // a service defined without an image or build context.
    assert!(
        spec.contains("neither an image nor a build context"),
        "the spec must use the real compose error text for a bad service, \
         not an invented string"
    );
    // The record the AC governs, plus the rollback outcome shown beside it.
    assert!(
        spec.contains(FIELD) && spec.contains("last_rollback"),
        "the spec must seed `{FIELD}` and `last_rollback` — the bundle and \
         the rollback outcome are what the browser must show"
    );
    // AC3 in the browser: the explicit statement, not an empty log section.
    assert!(
        spec.contains(NO_LOGS_FIELD) && spec.to_lowercase().contains("no container logs"),
        "the spec must cover the explicit no-container-logs statement"
    );
    // AC4 in the browser: a seeded secret must never render raw.
    assert!(
        spec.to_lowercase().contains("secret"),
        "the spec must assert the masked secret never renders (AC4's browser half)"
    );
    // House gates: console-error gate armed and asserted, golden screenshot.
    assert!(
        spec.contains("armConsoleGate") && spec.contains("assertNoConsoleErrors"),
        "the spec must arm and assert the shared console-error gate"
    );
    assert!(
        spec.contains("toHaveScreenshot"),
        "the spec must take a golden screenshot (house rule for UI changes)"
    );
    // Not parked — a skipped acceptance gate is a blind gate.
    for parked in ["test.fixme", "test.skip", "test.todo", "test.only"] {
        assert!(
            !spec.contains(parked),
            "the spec parks a test with `{parked}` — fix the behaviour instead"
        );
    }
}
