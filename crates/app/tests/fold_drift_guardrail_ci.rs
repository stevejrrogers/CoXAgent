#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! CXA-B302 — wire the drift guardrail into the dev-gate: pass, fail-with-
//! report and re-freeze-guidance paths, all over injected fake ports
//! (no host process, no disk).
//!
//! Like `fold_inventory_freeze_b281.rs`, this ALSO keeps the committed
//! golden file honest against the live manifest via the REAL adapters.

use coxagent_application::ports::outbound::fold_drift_source::FoldDriftSourcePort;
use coxagent_application::ports::outbound::fold_freeze_source::{
    DeclaredPanel, FoldFreezeSourcePort,
};
use coxagent_application::use_cases::{
    freeze_inventory, run_fold_drift_gate, FoldDriftVerdict, REFREEZE_COMMAND,
};
use coxagent_domain::{fnv1a_64_hex, PanelInventoryEntry};
use std::path::Path;

// ---- fake port double (in-memory only) -----------------------------------

struct FakeSide {
    frozen: Vec<PanelInventoryEntry>,
    live: Vec<PanelInventoryEntry>,
}
impl FoldDriftSourcePort for FakeSide {
    fn frozen_snapshot(&self) -> Result<Vec<PanelInventoryEntry>, String> {
        Ok(self.frozen.clone())
    }
    fn live_inventory(&self) -> Result<Vec<PanelInventoryEntry>, String> {
        Ok(self.live.clone())
    }
}

#[derive(Clone)]
struct Owned {
    slug: String,
}
/// The double's records are owned `String`s so tests can pass non-`'static`
/// slice literals straight in.
impl From<Owned> for DeclaredPanel {
    fn from(o: Owned) -> Self {
        let slug = o.slug;
        Self {
            dom_id: format!("ov-{slug}"),
            component: format!("{slug}.js paint{slug}() @ ov-{slug}"),
            visibility_rule_tokens: vec!["empty".to_owned(), "ready".to_owned()],
            slug,
        }
    }
}

fn freeze(slugs: &[&str]) -> Vec<PanelInventoryEntry> {
    let src = FreezeRows(
        slugs
            .iter()
            .map(|s| Owned {
                slug: (*s).to_owned(),
            })
            .collect(),
    );
    freeze_inventory(&src).unwrap().entries
}

/// Adapter holding owned rows so `freeze()` takes plain slices.
struct FreezeRows(Vec<Owned>);
impl FoldFreezeSourcePort for FreezeRows {
    fn declared_panels(&self) -> Result<Vec<DeclaredPanel>, String> {
        Ok(self.0.iter().map(|o| o.clone().into()).collect())
    }
}

/// Second fake for the real-wiring test: frozen = parsed committed golden,
/// live = freshly computed manifest (both held purely in memory here).
struct InMemory(Vec<PanelInventoryEntry>, Vec<PanelInventoryEntry>);
impl FoldDriftSourcePort for InMemory {
    fn frozen_snapshot(&self) -> Result<Vec<PanelInventoryEntry>, String> {
        Ok(self.0.clone())
    }
    fn live_inventory(&self) -> Result<Vec<PanelInventoryEntry>, String> {
        Ok(self.1.clone())
    }
}

// ---- AC1: gate passes when snapshot matches -------------------------------

#[test]
fn ac1_matching_inventory_passes_with_a_summary_count_and_clean_exit() {
    let inv = freeze(&["drain", "alerts"]);
    let verdict = run_fold_drift_gate(&FakeSide {
        frozen: inv.clone(),
        live: inv,
    })
    .unwrap();
    assert!(matches!(verdict, FoldDriftVerdict::Pass { .. }));
    assert!(
        verdict.clone().into_exit().is_ok(),
        "matching inventory must exit clean"
    );
    if let FoldDriftVerdict::Pass { summary } = &verdict {
        assert!(summary.contains("2 panels"), "count in summary: {summary}");
    }
}

// ---- AC2: gate fails with correct exit signal + formatted report ----------

#[test]
fn ac2_live_side_reorders_and_adds_fails_naming_each_panel_expected_vs_actual() {
    // Freeze says drain→alerts; live renders alerts first and adds health.
    let verdict = run_fold_drift_gate(&FakeSide {
        frozen: freeze(&["drain", "alerts"]),
        live: freeze(&["alerts", "drain", "health"]),
    })
    .unwrap();
    let report = verdict.into_exit().expect_err("drift must fail the gate");
    // Correct exit signal shape: non-empty failure text.
    assert!(!report.is_empty());
    // Summary count present in the headline.
    assert!(report.contains("DRIFTED (3 findings)"), "{report}");
    // Per-panel attribution, expected vs actual:
    assert!(
        report.contains("reordered-slot drain: position — expected `slot 1`, got `slot 2`"),
        "{report}"
    );
    assert!(
        report.contains("UNEXPECTED  health: expected (none), got rendered at slot 3"),
        "{report}"
    );
}

// ---- AC3: out-of-date snapshot instructs running the regenerate command ---

#[test]
fn ac3_out_of_date_snapshot_report_carries_the_exact_regenerate_command() {
    // Snapshot froze 4 panels; the live manifest shrank to 2 — every dropped
    // panel attributed with its expected slot.
    let report = run_fold_drift_gate(&FakeSide {
        frozen: freeze(&["drain", "alerts", "kpis", "releases"]),
        live: freeze(&["drain", "alerts"]),
    })
    .unwrap()
    .into_exit()
    .expect_err("stale snapshot must fail");
    assert!(
        report.contains("MISSING  kpis: expected rendered at slot 3, got (none)"),
        "{report}"
    );
    assert!(report.contains("MISSING  releases:"), "{report}");
    assert!(report.contains(REFREEZE_COMMAND), "{report}");
    assert!(report.contains("RE-FREEZE"), "{report}");
}

#[test]
fn an_unreadable_frozen_snapshot_is_a_loud_failure_not_drift() {
    struct Unreadable;
    impl FoldDriftSourcePort for Unreadable {
        fn frozen_snapshot(&self) -> Result<Vec<PanelInventoryEntry>, String> {
            Err("snapshot unreadable".into())
        }
        fn live_inventory(&self) -> Result<Vec<PanelInventoryEntry>, String> {
            Ok(vec![])
        }
    }
    let err = run_fold_drift_gate(&Unreadable).expect_err("observation error propagates");
    assert_eq!(err, "snapshot unreadable");
    // Sanity on a domain helper the pipeline leans on (pure, cheap):
    assert_eq!(fnv1a_64_hex(&["ready"]), fnv1a_64_hex(&["ready"]));
}

// ---- AC4: the REAL wiring stays honest (golden == live manifest) ----------

#[test]
fn ac4_real_adapters_still_agree_run_fold_drift_gate_passes_over_them() {
    use coxagent_presentation::fold_freeze_adapter::ManifestFreezeSource;
    let live: Vec<PanelInventoryEntry> = freeze_inventory(&ManifestFreezeSource).unwrap().entries;

    // The committed golden parsed through the same strict parser.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../presentation/__inventories__");
    let golden_raw = std::fs::read_to_string(root.join("above_the_fold_panels.golden")).unwrap();
    let golden: Vec<PanelInventoryEntry> = golden_raw
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(PanelInventoryEntry::parse_canonical)
        .collect::<Result<_, _>>()
        .unwrap();

    let verdict = run_fold_drift_gate(&InMemory(golden.clone(), live)).unwrap();
    if let FoldDriftVerdict::Fail { report } = &verdict {
        panic!("real wiring drifted:\n{report}");
    }
    assert_eq!(golden.len(), 7);
}
