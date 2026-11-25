//! CXA-B188 — DEPTH contracts for the above-the-fold Overview panels, over
//! the real view-model/state types that already carry the data.
//!
//! WHERE THIS SITS (read this before editing):
//! - CXA-B186a (`guardrail_scaffold_b201.rs`) = shared scaffold + the KPI
//!   panel STRUCTURAL exemplar; CXA-B187.2 (`panel_structural_contracts_b187_2.rs`)
//!   = structural contracts for the other five panels. Those assert markup
//!   SURVIVES. This file asserts the panels are DEEP (repo depth bar):
//!   every NUMBER carries its window label, a delta/trend where history
//!   exists in the type, and a zero-state hint at 0; every STATE carries
//!   attribution (who/when/why) and a WHY line for its off/paused variant;
//!   every ACTION carries an inverse/undo and, where the type models a
//!   destructive step, a confirm naming the blast radius.
//! - To ADD a panel entry: (1) find its renderer in
//!   `crates/presentation/src/web/js/` and the state type it reads in
//!   `crates/application/src/state/`; (2) write one `PanelDepthContract`
//!   whose `requires` are substrings of the CHECKED-IN renderer that prove
//!   each depth element (never a needle the code does not contain today);
//!   (3) add it to `depth_contracts()`; (4) if a depth element genuinely has
//!   no data in any existing type, DO NOT invent one and DO NOT touch
//!   production types — skip it and record the gap as
//!   `ASK SA: <panel>: <field>` in the ticket report (the no-fabrication
//!   rule). A contract whose needle is not already in the source fails the
//!   live gate, by design — that is how fabrication is refused.
//! - Enforcement is STRICT and lives with the layout guard in
//!   `crates/app/tests/guardrail_registry_gate.rs`: a registry entry with no
//!   structural or depth coverage fails CI naming the panel.

// Test-binary lints: unwrap-family is the point of a failing gate assertion.
#![allow(clippy::unwrap_in_result, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Port — the only IO a depth check may perform (CXA-B186a pattern)
// ---------------------------------------------------------------------------

/// Outbound port: read a UTF-8 file relative to the repo root.
trait ReadRepoText {
    fn read_utf8(&self, rel: &str) -> Option<String>;
}

/// One panel's depth contract: needles that prove the depth elements the
/// panel renders today. Every needle is a real substring of the checked-in
/// source, so the contract can only pass while the depth survives.
struct PanelDepthContract {
    /// Stable name — what CI prints on failure.
    name: &'static str,
    /// The panel this contract speaks for.
    panel: &'static str,
    /// Repo-relative renderer file the needles live in.
    file: &'static str,
    /// Which depth-bar kind this panel's entries are (label only, for the
    /// failure message).
    kind: &'static str,
    /// Substrings that must survive for the panel to stay deep.
    requires: &'static [&'static str],
}

impl PanelDepthContract {
    /// The pure decision for one panel over one file read. `Err` names the
    /// panel, the missing depth element kind and each lost needle.
    fn check(&self, read: &dyn ReadRepoText) -> Result<(), String> {
        let Some(src) = read.read_utf8(self.file) else {
            return Err(format!(
                "[{}] {} (`{}`) is not readable — refusing to guess (fail closed)",
                self.name, self.panel, self.file
            ));
        };
        let lost: Vec<&str> = self
            .requires
            .iter()
            .copied()
            .filter(|n| !src.contains(n))
            .collect();
        if lost.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "[{}] {} (`{}`) lost {} depth ({})\n{}",
                self.name,
                self.panel,
                self.file,
                self.kind,
                self.name,
                lost.iter()
                    .map(|n| format!("  - lost needle: `{n}`"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// The contracts — one per above-the-fold panel with assertable depth
// ---------------------------------------------------------------------------

const KPI_JS: &str = "crates/presentation/src/web/js/kpis.js";
const MCP_JS: &str = "crates/presentation/src/web/js/mcp.js";
const SHELL_JS: &str = "crates/presentation/src/web/js/shell.js";

/// KPI tile strip (`#kpis`, CXA-F360): each tile is a NUMBER with its window
/// (the 14-day `series:` spark), a signed delta vs the prior 14 days, and a
/// zero-state hint (`hint:`) for the tile-at-0 case.
fn kpis_depth_contract() -> PanelDepthContract {
    PanelDepthContract {
        name: "overview/kpis-numbers-carry-window-delta-zero-state",
        panel: "KPI tile strip (#kpis)",
        file: KPI_JS,
        kind: "number",
        requires: &[
            "overviewKpiTile({",
            "series:", // the 14-day window (sparkline data per tile)
            "hint:",   // zero-state hint rendered when the tile reads 0
            "go:",     // click-through drill-down
            "14-day",  // the window label the tiles are labelled with
        ],
    }
}

/// Run-control state line (renders `WorkspaceRun`, `state/mod.rs`): a STATE
/// that can be off. The paused variant must carry attribution — WHO paused
/// (`ws.by`, the `WorkspaceRun::by` field) and WHY (`ws.reason`) — never a
/// bare "paused".
fn run_control_depth_contract() -> PanelDepthContract {
    PanelDepthContract {
        name: "overview/run-control-state-carries-attribution",
        panel: "Run-control state line (WorkspaceRun)",
        file: MCP_JS,
        kind: "state",
        requires: &[
            "window.STATE.workspace_run", // reads the real state type
            "paused by ",                 // the off-state WHY/attribution lead-in
            "ws.by",                      // WHO flipped it last
            "ws.reason",                  // WHY it is paused
        ],
    }
}

/// Drain banner (`#ov-drain`, `shell.js`): the paused variant of the
/// workspace must say WHY the hold exists (refactor waiting on a clean
/// merge), the way out, and the token-free ACTION with its effect named.
fn drain_depth_contract() -> PanelDepthContract {
    PanelDepthContract {
        name: "overview/drain-banner-names-why-and-the-way-out",
        panel: "Drain banner (#ov-drain)",
        file: SHELL_JS,
        kind: "state+action",
        requires: &[
            "All agents for all users PAUSE new work", // the WHY, verbatim
            "Open Review to merge green PRs",          // the way out (inverse)
            "mergeSweep()",                            // the ACTION's entrypoint
            "SA merge sweep",                          // the ACTION's label
            "no tokens",                               // the action's blast radius
        ],
    }
}

fn depth_contracts() -> Vec<PanelDepthContract> {
    vec![
        kpis_depth_contract(),
        run_control_depth_contract(),
        drain_depth_contract(),
    ]
}

/// The batch decision: every depth contract passes, or one failure naming
/// each shallow panel. An empty batch is a failure, not a green light.
fn check_depth_batch(
    contracts: &[PanelDepthContract],
    read: &dyn ReadRepoText,
) -> Result<(), String> {
    if contracts.is_empty() {
        return Err("no depth contracts registered — nothing is guarded".to_owned());
    }
    let failed: Vec<String> = contracts
        .iter()
        .filter_map(|c| c.check(read).err())
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n---\n"))
    }
}

// ---------------------------------------------------------------------------
// Adapter — the only place this file touches the real filesystem
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../")
}

/// The real repo, behind the port.
struct Workspace {
    root: PathBuf,
}

impl ReadRepoText for Workspace {
    fn read_utf8(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.root.join(rel)).ok()
    }
}

/// The never-readable repo — fail-closed self-test fixture.
struct NullRepo;
impl ReadRepoText for NullRepo {
    fn read_utf8(&self, _rel: &str) -> Option<String> {
        None
    }
}

/// The live reader with chosen files replaced in memory — the negative tests
/// mutate a renderer the way a careless refactor would, WITHOUT touching the
/// working tree.
struct OverlaidRepo {
    over: std::collections::BTreeMap<String, String>,
    inner: Workspace,
}

impl ReadRepoText for OverlaidRepo {
    fn read_utf8(&self, rel: &str) -> Option<String> {
        if let Some(text) = self.over.get(rel) {
            return Some(text.clone());
        }
        self.inner.read_utf8(rel)
    }
}

// ---------------------------------------------------------------------------
// Self-tests — the contracts prove their own failure paths first
// ---------------------------------------------------------------------------

#[test]
fn a_missing_renderer_fails_closed() {
    let why = kpis_depth_contract()
        .check(&NullRepo)
        .expect_err("an unreadable renderer must fail, not pass");
    assert!(why.contains("not readable"), "unhelpful: {why}");
}

#[test]
fn an_empty_batch_is_a_failure_not_a_green_light() {
    let why = check_depth_batch(&[], &NullRepo).expect_err("nothing guarded must fail");
    assert!(why.contains("no depth contracts"), "unhelpful: {why}");
}

#[test]
fn a_number_that_lost_its_zero_state_hint_is_caught() {
    let repo = OverlaidRepo {
        over: [(
            KPI_JS.to_owned(),
            "overviewKpiTile({label:\"x\",series:[1],go:nav()})".to_owned(),
        )]
        .into_iter()
        .collect(),
        inner: Workspace { root: repo_root() },
    };
    let why = kpis_depth_contract()
        .check(&repo)
        .expect_err("a tile without hint: must fail");
    assert!(why.contains("hint:"), "names the lost depth element: {why}");
    assert!(why.contains("number"), "names the kind: {why}");
    assert!(
        why.contains("kpis-numbers-carry-window-delta-zero-state"),
        "names the contract: {why}"
    );
}

#[test]
fn a_state_that_lost_its_attribution_is_caught() {
    // The exact regression the depth bar exists for: the pause pill degrades
    // to a bare "paused" with no who and no why.
    let bare = "const pill=ws?(\"paused\"):(\"running\");";
    let repo = OverlaidRepo {
        over: [(MCP_JS.to_owned(), bare.to_owned())].into_iter().collect(),
        inner: Workspace { root: repo_root() },
    };
    let why = run_control_depth_contract()
        .check(&repo)
        .expect_err("an unattributed pause must fail");
    assert!(why.contains("ws.by"), "names WHO: {why}");
    assert!(why.contains("ws.reason"), "names WHY: {why}");
    assert!(why.contains("state"), "names the kind: {why}");
}

#[test]
fn an_action_that_lost_its_labelled_entrypoint_is_caught() {
    let repo = OverlaidRepo {
        over: [(
            SHELL_JS.to_owned(),
            "<div class=\"drainbar\">All agents for all users PAUSE new work</div>".to_owned(),
        )]
        .into_iter()
        .collect(),
        inner: Workspace { root: repo_root() },
    };
    let why = drain_depth_contract()
        .check(&repo)
        .expect_err("a hold with no way out must fail");
    assert!(why.contains("mergeSweep()"), "names the lost action: {why}");
}

// ---------------------------------------------------------------------------
// Live gates — the checked-in renderers, through the real adapter
// ---------------------------------------------------------------------------

#[test]
fn the_batch_declares_the_assertable_depth_panels() {
    let contracts = depth_contracts();
    assert_eq!(
        contracts.len(),
        3,
        "kpis (number), run-control (state) and drain (state+action) are the \
         panels whose view-model types already carry depth data; growing the \
         batch means a NEW type first, then a contract"
    );
    for c in &contracts {
        assert!(c.file.starts_with("crates/presentation/src/web/js/"));
    }
}

#[test]
fn the_checked_in_panels_obey_their_depth_contracts() {
    let ws = Workspace { root: repo_root() };
    check_depth_batch(&depth_contracts(), &ws)
        .unwrap_or_else(|why| panic!("an above-the-fold panel lost its depth:\n{why}"));
}
