//! CXA-F226 acceptance gate — "CXA-F223c: Drift alert pass + dashboard
//! surfacing (from CXA-F223)".
//!
//! Written before implementation (TDD) so the acceptance criteria are pinned
//! as executable invariants over the existing state/domain types; compiles
//! today and fails only for the missing behaviour. PURE: no fake HTTP server,
//! no host harness, no network port — the scan runs over in-memory port
//! doubles (the same pattern conformance_check.rs's unit tests use).
//!
//! HOW THE CRITERIA MAP TO CODE THAT EXISTS (grounded, not guessed):
//! * "`coxagent check --work_dir <repo>`" IS `RunConformanceUseCase` — the
//!   exact pass `Command::Check` wraps (crates/app/src/lib.rs) with the real
//!   filesystem adapter. Here the identical pass runs over the in-memory
//!   files port, so the decision path under test is the production one minus
//!   the disk.
//! * The "web dashboard" renders the serialized `ProjectState`:
//!   `lite_state_value` (server/status.rs) forwards the serialized state
//!   wholesale to the view, and existing alert surfaces (`engine_incidents`)
//!   are read straight off that object (web/js/chat.js). So the dashboard
//!   data contract for a drift alert is a field on the serialized state, and
//!   "cleared from both storage and the dashboard" is one observation: the
//!   dashboard consumes exactly what storage serializes.
//!
//! NAMES, AND WHERE THEY COME FROM (pinned so implementer and reviewer share
//! one contract; every name is the criteria's own word or a house precedent):
//! * `drift_alerts` — the state field: a `Vec` serialized whole to the
//!   dashboard like its closest precedent `engine_incidents` (a plain
//!   `#[serde(default)]` list, PRESENT even when empty — that presence is
//!   AC5's "reflects zero rather than hiding or going stale").
//! * Each entry carries `area` + `message` — the dedupe key's own words
//!   ("deduplicated by (area, message)") — and `ticket`, the id of the filed
//!   bug the entry links to (house precedent: `DeployRecord.ticket`,
//!   `IncidentRecord.root_cause_ticket`).
//! * The visible heading for an area is the existing `Violation::bug_title()`
//!   format — the AC's own example "Architecture drift in server".
//!
//! Red today, and why:
//!   * AC1 — no scan record survives into state: the serialized state has no
//!     drift-alert surface at all.
//!   * AC2 — no entry carries area/message/ticket, and no dashboard script
//!     renders drift alerts or links them through the house `showTicket(...)`
//!     shortcut every other entry card uses (core.js, inbox.js).
//!   * AC3 — nothing dedupes alerts by (area, message) (bug filing dedupes by
//!     title only — a different, coarser key).
//!   * AC4 — a resolving scan never revisits state: `execute()` returns
//!     before saving when it finds no violations, so nothing can clear.
//!   * AC5 — no always-present-at-zero aggregate surface, and no dashboard
//!     indicator consuming it.
//!
//! View-side pins are source scans of the dashboard scripts (the
//! openapi_routes_gate.rs / fleet_river_f233_gate.rs pattern) because the
//! rendered pixels are gated end-to-end elsewhere (AGENTS.md: any UI change
//! must pass `cd e2e && npx playwright test`); the data those pixels consume
//! is pinned here over the real types.
//!
//! AC → test map:
//! - AC1: [`ac1_a_violating_check_produces_a_drift_alert_naming_area_and_message_and_files_bugs`],
//!   [`ac1_a_fully_conformant_check_produces_no_new_alert`]
//! - AC2: [`ac2_each_open_alert_carries_the_area_the_message_and_its_filed_bug_ticket`],
//!   [`ac2_the_dashboard_renders_each_alert_as_an_entry_linked_to_its_filed_ticket`]
//! - AC3: [`ac3_re_checking_an_identical_violation_does_not_duplicate_alerts`],
//!   [`ac3_distinct_violations_in_one_area_are_distinct_alerts`]
//! - AC4: [`ac4_a_resolved_areas_alerts_are_cleared_automatically_while_open_areas_remain`]
//! - AC5: [`ac5_the_aggregate_surface_reflects_zero_after_a_fully_conformant_check`],
//!   [`ac5_the_dashboard_shows_an_aggregate_open_drift_alert_indicator`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use coxagent_application::conformance::{self, StackRule, Violation};
use coxagent_application::ports::outbound::{FileMeta, StateStorePort, WorkspaceFilesPort};
use coxagent_application::use_cases::RunConformanceUseCase;
use coxagent_application::{PortError, ProjectState};
use coxagent_domain::{Status, TicketId, TicketType};

// --- pure doubles over the real ports (the conformance_check.rs pattern) ----

#[derive(Default)]
struct MemStore {
    state: Mutex<ProjectState>,
}

#[async_trait::async_trait]
impl StateStorePort for MemStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().expect("lock").clone())
    }

    async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
        s.validate().map_err(PortError::Corrupt)?;
        *self.state.lock().expect("lock") = s.clone();
        Ok(())
    }
}

/// In-memory workspace: `list_recursive` answers from a fixed path list.
struct FixedFiles(Vec<PathBuf>);

#[async_trait::async_trait]
impl WorkspaceFilesPort for FixedFiles {
    async fn read(&self, _: &Path) -> Option<String> {
        None
    }
    async fn read_bytes(&self, _: &Path) -> Option<Vec<u8>> {
        None
    }
    async fn write(&self, _: &Path, _: &str) -> bool {
        false
    }
    async fn write_bytes(&self, _: &Path, _: &[u8]) -> bool {
        false
    }
    async fn delete(&self, _: &Path) -> bool {
        false
    }
    async fn stat(&self, _: &Path) -> Option<FileMeta> {
        None
    }
    async fn list_recursive(&self, dir: &Path) -> Vec<PathBuf> {
        self.0
            .iter()
            .filter(|p| p.starts_with(dir))
            .cloned()
            .collect()
    }
    async fn list_dirs(&self, _: &Path) -> Vec<PathBuf> {
        Vec::new()
    }
    async fn list(&self, _: &Path) -> Vec<FileMeta> {
        Vec::new()
    }
}

// --- fixtures over the real state/domain types ------------------------------

const ROOT: &str = "/w";

/// One marker requirement + one forbidden extension, so a tree can violate a
/// rule once (forbidden ext, marker present) or twice (both, distinct
/// messages) — the two shapes AC3's dedupe key must tell apart.
fn rust_server_rule() -> StackRule {
    StackRule {
        area: "server".to_owned(),
        language: "Rust".to_owned(),
        require_any: vec!["Cargo.toml".to_owned()],
        forbid_ext: vec![".ts".to_owned()],
    }
}

/// A second area, so AC4 can resolve one area while another stays open.
fn ts_web_rule() -> StackRule {
    StackRule {
        area: "web".to_owned(),
        language: "TypeScript".to_owned(),
        require_any: vec!["package.json".to_owned()],
        forbid_ext: vec![".rs".to_owned()],
    }
}

fn paths(rels: &[&str]) -> Vec<PathBuf> {
    rels.iter().map(|r| Path::new(ROOT).join(r)).collect()
}

/// The violations the pure conformance check produces for `area_files` under
/// `rules` — the expected alert content, derived from the real function the
/// pass runs, never a hardcoded string.
fn expected_violations(rules: &[StackRule], area_files: &[(&str, Vec<PathBuf>)]) -> Vec<Violation> {
    let mut by_area = BTreeMap::new();
    for (area, files) in area_files {
        by_area.insert(
            (*area).to_owned(),
            files
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<String>>(),
        );
    }
    conformance::check(&by_area, rules)
}

/// One `coxagent check` pass over the in-memory workspace, then the state
/// exactly as the dashboard receives it: the serialized `ProjectState`.
async fn run_check(
    store: &Arc<MemStore>,
    rules: Vec<StackRule>,
    files: Vec<PathBuf>,
) -> (Vec<TicketId>, serde_json::Value) {
    let uc = RunConformanceUseCase::new(Arc::clone(store), PathBuf::from(ROOT), rules)
        .with_files(Some(Arc::new(FixedFiles(files))));
    let bug_ids = uc.execute().await.expect("the conformance pass runs");
    let state = store.load().await.expect("state loads");
    let json = serde_json::to_value(&state).expect("state serializes");
    (bug_ids, json)
}

/// The serialized drift-alert surface: `Some` (present, even when empty) once
/// the behaviour exists. Absence is the red state today — and after
/// implementation, absence at zero violations is exactly the "hiding or going
/// stale" failure AC5 forbids.
fn alerts_field(json: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    json.get("drift_alerts")
        .and_then(serde_json::Value::as_array)
}

// --- dashboard view pins (source scans; the fleet_river_f233_gate pattern) --

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
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

/// The script(s) that render the drift-alert section — pinned by the AC's own
/// name for the feature ("drift alert"), since no codebase identifier exists
/// for the view yet.
fn drift_alert_scripts() -> Vec<(String, String)> {
    web_js_sources()
        .into_iter()
        .filter(|(_, src)| src.to_lowercase().contains("drift alert"))
        .collect()
}

// --- AC1 --------------------------------------------------------------------

/// AC1: "Running `coxagent check --work_dir <repo>` against a codebase with at
/// least one conformance violation produces a visible drift alert naming the
/// violating area and message, in addition to filing its bug ticket(s)".
/// "Visible" = surfaced to the dashboard per AC2/AC5: the alert record in the
/// state the dashboard renders, naming the area and the violation message.
#[tokio::test]
async fn ac1_a_violating_check_produces_a_drift_alert_naming_area_and_message_and_files_bugs() {
    let store = Arc::new(MemStore::default());
    let rules = vec![rust_server_rule()];
    // Marker present, forbidden extension present: exactly one violation.
    let files = paths(&["server/Cargo.toml", "server/src/index.ts"]);
    let expected = expected_violations(&rules, &[("server", files.clone())]);
    assert_eq!(expected.len(), 1, "fixture: exactly one violation");

    let (bug_ids, json) = run_check(&store, rules, files).await;

    assert!(
        !bug_ids.is_empty(),
        "the pass still files its bug ticket(s) for the violation"
    );
    let alerts = alerts_field(&json)
        .expect("a violating check leaves a drift-alert surface in the rendered state");
    assert_eq!(alerts.len(), 1, "one violation, one open alert: {alerts:?}");
    assert_eq!(
        alerts[0]["area"].as_str(),
        Some(expected[0].area.as_str()),
        "the alert names the violating area"
    );
    assert_eq!(
        alerts[0]["message"].as_str(),
        Some(expected[0].message.as_str()),
        "the alert names the violation message, verbatim from the check"
    );
}

/// AC1 (second half): "running it against a fully conformant tree produces no
/// new alert". Fresh project, clean tree, scanned-and-clean area (marker
/// present, nothing forbidden) — no alert may appear.
#[tokio::test]
async fn ac1_a_fully_conformant_check_produces_no_new_alert() {
    let store = Arc::new(MemStore::default());
    let rules = vec![rust_server_rule()];
    let files = paths(&["server/Cargo.toml", "server/src/main.rs"]);
    assert!(
        expected_violations(&rules, &[("server", files.clone())]).is_empty(),
        "fixture: the tree is fully conformant"
    );

    let (bug_ids, json) = run_check(&store, rules, files).await;

    assert!(bug_ids.is_empty(), "nothing to file on a conformant tree");
    let alerts = alerts_field(&json)
        .expect("the alert surface exists even on a clean scan (never absent/stale)");
    assert!(
        alerts.is_empty(),
        "a fully conformant tree raises no alert: {alerts:?}"
    );
}

// --- AC2 --------------------------------------------------------------------

/// AC2: "Each open drift alert appears on the web dashboard as its own entry
/// showing the area (e.g. "Architecture drift in server"), the violation
/// message, and a link/shortcut to the filed bug ticket for that violation."
/// Data half: the entry carries area + message + the filed bug's id, and that
/// bug is a real OPEN ticket titled exactly the AC's example heading for the
/// area (the existing `Violation::bug_title()` format).
#[tokio::test]
async fn ac2_each_open_alert_carries_the_area_the_message_and_its_filed_bug_ticket() {
    let store = Arc::new(MemStore::default());
    let rules = vec![rust_server_rule()];
    let files = paths(&["server/Cargo.toml", "server/src/index.ts"]);
    let expected = expected_violations(&rules, &[("server", files.clone())]);

    let (bug_ids, json) = run_check(&store, rules, files).await;

    let alerts = alerts_field(&json).expect("drift-alert surface");
    assert_eq!(alerts.len(), 1, "one violation, one entry: {alerts:?}");
    let alert = &alerts[0];
    assert_eq!(
        alert["area"].as_str(),
        Some(expected[0].area.as_str()),
        "the entry shows the area"
    );
    assert_eq!(
        alert["message"].as_str(),
        Some(expected[0].message.as_str()),
        "the entry shows the violation message"
    );

    let linked = alert["ticket"]
        .as_str()
        .expect("the entry links/shortcuts to the filed bug ticket by id");
    assert!(
        bug_ids.iter().any(|id| id.as_str() == linked),
        "the linked ticket {linked} is one this scan filed"
    );
    let state = store.load().await.expect("state loads");
    let bug = state
        .tickets
        .iter()
        .find(|t| t.id().as_str() == linked)
        .expect("the linked ticket exists in state");
    assert_eq!(
        bug.ticket_type(),
        TicketType::Bug,
        "the link targets the filed bug"
    );
    assert_eq!(bug.status(), Status::Open, "the linked bug is open");
    assert_eq!(
        bug.title(),
        expected[0].bug_title(),
        "the area's visible heading is the bug-title format, e.g. \"Architecture drift in server\""
    );
}

/// AC2 (view half): the dashboard renders drift-alert entries and links each
/// to its filed ticket through the house ticket opener (`showTicket(...)`) —
/// the same shortcut every other entry card on this dashboard uses
/// (core.js, inbox.js). The heading/message wording itself is pinned at the
/// data layer above and visually by the e2e golden suite.
#[test]
fn ac2_the_dashboard_renders_each_alert_as_an_entry_linked_to_its_filed_ticket() {
    let scripts = drift_alert_scripts();
    assert!(
        !scripts.is_empty(),
        "no dashboard script renders drift alerts — the entries never appear"
    );
    assert!(
        scripts.iter().any(|(_, src)| src.contains("showTicket(")),
        "each alert entry must link/shortcut to its filed bug ticket via the house \
         ticket opener showTicket(...), like every other entry card on this dashboard"
    );
}

// --- AC3 --------------------------------------------------------------------

/// AC3: "Alerts are deduplicated by (area, message): re-running `coxagent
/// check` while an identical violation still exists does not create duplicate
/// alerts or duplicate dashboard entries."
#[tokio::test]
async fn ac3_re_checking_an_identical_violation_does_not_duplicate_alerts() {
    let store = Arc::new(MemStore::default());
    let rules = vec![rust_server_rule()];
    let files = paths(&["server/Cargo.toml", "server/src/index.ts"]);
    let expected = expected_violations(&rules, &[("server", files.clone())]);
    assert_eq!(expected.len(), 1, "fixture: one standing violation");

    let (_, first) = run_check(&store, rules.clone(), files.clone()).await;
    let first_alerts = alerts_field(&first).expect("drift-alert surface after the first check");
    assert_eq!(first_alerts.len(), 1, "the first check opens the alert");

    let (_, second) = run_check(&store, rules, files).await;
    let alerts = alerts_field(&second).expect("the alert surface survives the re-check");
    assert_eq!(
        alerts.len(),
        1,
        "the identical violation still exists, so exactly one alert remains (no duplicate): {alerts:?}"
    );
    assert_eq!(alerts[0]["area"].as_str(), Some(expected[0].area.as_str()));
    assert_eq!(
        alerts[0]["message"].as_str(),
        Some(expected[0].message.as_str())
    );
}

/// AC3 (key granularity): the dedupe key is the (area, message) PAIR, not the
/// area alone — two distinct violations in the SAME area are two distinct
/// open alerts. The fixture trips the marker rule and the forbidden-ext rule
/// at once, producing two different messages for `server`.
#[tokio::test]
async fn ac3_distinct_violations_in_one_area_are_distinct_alerts() {
    let store = Arc::new(MemStore::default());
    let rules = vec![rust_server_rule()];
    let files = paths(&["server/src/index.ts"]);
    let expected = expected_violations(&rules, &[("server", files.clone())]);
    assert_eq!(
        expected.len(),
        2,
        "fixture: two distinct violations in one area"
    );

    let (_, json) = run_check(&store, rules, files).await;

    let alerts = alerts_field(&json).expect("drift-alert surface");
    assert_eq!(
        alerts.len(),
        2,
        "one alert per (area, message), not per area: {alerts:?}"
    );
    let mut got: Vec<(String, String)> = alerts
        .iter()
        .map(|a| {
            (
                a["area"].as_str().unwrap_or_default().to_owned(),
                a["message"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    got.sort();
    let mut want: Vec<(String, String)> = expected
        .iter()
        .map(|v| (v.area.clone(), v.message.clone()))
        .collect();
    want.sort();
    assert_eq!(got, want, "alerts match the violations pairwise");
}

// --- AC4 --------------------------------------------------------------------

/// AC4: "When every violation for an area is resolved — i.e. a subsequent
/// `coxagent check` finds no violations there — that area's open alerts are
/// automatically cleared from both storage and the dashboard; no manual
/// dismissal is required." The follow-up check runs with NO dismissal call:
/// server gets fixed while web stays violating, so exactly web's alert
/// survives — in the state storage holds and the dashboard renders.
#[tokio::test]
async fn ac4_a_resolved_areas_alerts_are_cleared_automatically_while_open_areas_remain() {
    let store = Arc::new(MemStore::default());
    let rules = vec![rust_server_rule(), ts_web_rule()];

    let dirty: Vec<PathBuf> = {
        let mut f = paths(&["server/Cargo.toml", "server/src/index.ts"]);
        f.extend(paths(&["web/package.json", "web/legacy.rs"]));
        f
    };
    let server_fixed: Vec<PathBuf> = {
        let mut f = paths(&["server/Cargo.toml", "server/src/main.rs"]);
        f.extend(paths(&["web/package.json", "web/legacy.rs"]));
        f
    };
    let dirty_expected = expected_violations(
        &rules,
        &[
            (
                "server",
                paths(&["server/Cargo.toml", "server/src/index.ts"]),
            ),
            ("web", paths(&["web/package.json", "web/legacy.rs"])),
        ],
    );
    let web_expected = expected_violations(
        &rules,
        &[("web", paths(&["web/package.json", "web/legacy.rs"]))],
    );
    assert_eq!(dirty_expected.len(), 2, "fixture: both areas violate");
    assert_eq!(web_expected.len(), 1, "fixture: only web still violates");

    let (_, after_dirty) = run_check(&store, rules.clone(), dirty).await;
    let open = alerts_field(&after_dirty).expect("drift-alert surface");
    assert_eq!(
        open.len(),
        2,
        "both areas are open after the first check: {open:?}"
    );

    let (_, after_fix) = run_check(&store, rules, server_fixed).await;
    let remaining =
        alerts_field(&after_fix).expect("the alert surface persists across the resolving check");
    assert_eq!(
        remaining.len(),
        1,
        "server's resolved alert is auto-cleared; only the still-violating area keeps its alert: {remaining:?}"
    );
    assert_eq!(
        remaining[0]["area"].as_str(),
        Some("web"),
        "the surviving alert belongs to the still-violating area"
    );
    assert_eq!(
        remaining[0]["message"].as_str(),
        Some(web_expected[0].message.as_str()),
        "the surviving alert is web's violation message"
    );
}

// --- AC5 --------------------------------------------------------------------

/// AC5 (data half): "The dashboard shows an aggregate indicator of open drift
/// alerts (count badge / section header) that updates after each scan; with
/// zero violations across all areas it reflects zero rather than hiding or
/// going stale." The indicator counts the always-present `drift_alerts`
/// array (the `engine_incidents` convention: a plain `#[serde(default)]`
/// list, serialized even when empty). This pins both ends: the count after a
/// dirty scan, and the surface still present — reading zero — after a fully
/// conformant one.
#[tokio::test]
async fn ac5_the_aggregate_surface_reflects_zero_after_a_fully_conformant_check() {
    let store = Arc::new(MemStore::default());
    let rules = vec![rust_server_rule(), ts_web_rule()];

    let dirty: Vec<PathBuf> = {
        let mut f = paths(&["server/Cargo.toml", "server/src/index.ts"]);
        f.extend(paths(&["web/package.json", "web/legacy.rs"]));
        f
    };
    let all_clean: Vec<PathBuf> = {
        let mut f = paths(&["server/Cargo.toml", "server/src/main.rs"]);
        f.extend(paths(&["web/package.json", "web/app.js"]));
        f
    };
    assert!(
        expected_violations(
            &rules,
            &[
                (
                    "server",
                    paths(&["server/Cargo.toml", "server/src/main.rs"])
                ),
                ("web", paths(&["web/package.json", "web/app.js"])),
            ],
        )
        .is_empty(),
        "fixture: the second tree is fully conformant"
    );

    let (_, after_dirty) = run_check(&store, rules.clone(), dirty).await;
    let open = alerts_field(&after_dirty).expect("drift-alert surface");
    assert_eq!(
        open.len(),
        2,
        "the aggregate counts both open alerts after the dirty scan: {open:?}"
    );

    let (_, after_clean) = run_check(&store, rules, all_clean).await;
    let zero = alerts_field(&after_clean).expect(
        "at zero violations the surface must still be present — an absent field \
         reads as hidden/stale, not zero",
    );
    assert_eq!(
        zero.len(),
        0,
        "the aggregate reflects zero open drift alerts across all areas"
    );
}

/// AC5 (view half): the dashboard script that renders drift alerts also shows
/// the aggregate — a count badge / section header derived from the
/// always-present array (`.length` is how this dashboard counts a list), so
/// zero renders as zero instead of the section going stale or hiding.
#[test]
fn ac5_the_dashboard_shows_an_aggregate_open_drift_alert_indicator() {
    let scripts = drift_alert_scripts();
    assert!(
        !scripts.is_empty(),
        "no dashboard script renders drift alerts — there is no indicator to update"
    );
    assert!(
        scripts.iter().any(|(_, src)| src.contains(".length")),
        "the aggregate indicator (count badge / section header) must consume the \
         always-present drift_alerts array so zero renders as zero rather than hiding"
    );
}
