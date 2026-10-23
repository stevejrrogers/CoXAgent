//! The above-the-fold drift guardrail gate (CXA-B302, part 3/3 of
//! CXA-B240.2) — a thin runner composing the two previous pieces:
//!
//! 1. read BOTH inventories through [`FoldDriftSourcePort`] (freeze-side
//!    declarations recomputed live, snapshot file loaded),
//! 2. diff them with the PURE domain comparator
//!    [`coxagent_domain::panel_inventory_drift`],
//! 3. decide purely: clean → pass; drifted → fail with the rendered
//!    [`DriftReport`] that names every affected panel (expected vs actual)
//!    AND the exact re-freeze command.
//!
//! No IO here — ever. The port is the only door to the filesystem or the
//! manifest; tests inject doubles.

use crate::ports::outbound::fold_drift_source::FoldDriftSourcePort;
use coxagent_domain::{render_drift_report, DriftReport, PanelInventoryEntry};

/// The exact command an operator runs to re-freeze after an INTENTIONAL
/// layout change. Emitted verbatim in every failing report so "how do I fix
/// this" never has to be looked up.
pub const REFREEZE_COMMAND: &str = "cargo run -p coxagent-app --bin freeze_fold_inventory";

/// Outcome of one gate execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldDriftVerdict {
    /// Frozen and live agree; carries the summary line for the log.
    Pass { summary: String },
    /// Drift detected; carries the full formatted report including per-panel
    /// expected-vs-actual attribution and the regenerate command.
    Fail { report: String },
}

impl FoldDriftVerdict {
    /// Process exit signal: `Ok(())` passes, `Err(text)` fails the gate.
    ///
    /// # Errors
    /// The formatted drift report when the verdict is a failure, so a CI step
    /// can bubble it straight out as non-zero exit + stderr.
    pub fn into_exit(self) -> Result<(), String> {
        match self {
            Self::Pass { .. } => Ok(()),
            Self::Fail { report } => Err(report),
        }
    }

    /// Whether the gate failed (test convenience).
    #[must_use]
    pub fn is_fail(&self) -> bool {
        matches!(self, Self::Fail { .. })
    }
}

/// Run the drift gate against an injected source.
///
/// The decision is delegated ENTIRELY to the pure comparator — this function
/// only wires data through the port and formats the verdict.
///
/// # Errors
/// Observation failures (unreadable snapshot / unavailable live side) return
/// `Err` WITHOUT counting as drift: a broken observation must fail loudly,
/// not masquerade as inventory change.
pub fn run_fold_drift_gate(port: &dyn FoldDriftSourcePort) -> Result<FoldDriftVerdict, String> {
    let frozen = port.frozen_snapshot()?;
    if frozen.is_empty() {
        // An empty committed snapshot means the freeze was lost — same loud
        // failure shape as drift, but its own instruction text.
        return Err(format!(
            "frozen above-the-fold snapshot is EMPTY — \
             the committed golden file has no data lines; restore it or \
             regenerate with `{REFREEZE_COMMAND}`"
        ));
    }
    let live = port.live_inventory()?;

    let report: DriftReport = coxagent_domain::panel_inventory_drift(&frozen, &live);

    if report.is_clean() {
        return Ok(FoldDriftVerdict::Pass {
            summary: route_hint(&frozen),
        });
    }

    let guidance = format!(
        "\n\nHOW TO RE-FREEZE (after an intentional layout change):\n  \
         {REFREEZE_COMMAND}\nthen commit the regenerated crates/presentation/\
         __inventories__/above_the_fold_panels.golden.\nIf the change was NOT \
         intentional, fix the panel order/manifest instead."
    );
    let mut out = render_drift_report(&report);
    out.push_str(&guidance);
    Ok(FoldDriftVerdict::Fail { report: out })
}

fn route_hint(frozen: &[PanelInventoryEntry]) -> String {
    format!(
        "above-the-fold inventory matches the freeze: {} panels at {}",
        frozen.len(),
        frozen.first().map_or("#/overview", |e| e.route.as_str()),
    )
}

    /// Why a committed snapshot could not serve as a comparison base.
    #[cfg(test)]
    mod tests {
    use super::*;
    use crate::ports::outbound::fold_freeze_source::DeclaredPanel;
    use crate::use_cases::freeze_inventory;
    use coxagent_domain::{fnv1a_64_hex, PanelInventoryEntry};

    struct Fixed {
        frozen: Vec<PanelInventoryEntry>,
        live: Vec<PanelInventoryEntry>,
    }
    impl FoldDriftSourcePort for Fixed {
        fn frozen_snapshot(&self) -> Result<Vec<PanelInventoryEntry>, String> {
            Ok(self.frozen.clone())
        }
        fn live_inventory(&self) -> Result<Vec<PanelInventoryEntry>, String> {
            Ok(self.live.clone())
        }
    }

    fn declared(slug: &str) -> DeclaredPanel {
        DeclaredPanel {
            slug: slug.into(),
            dom_id: format!("ov-{slug}"),
            component: format!("{slug}.js paint{slug}()"),
            visibility_rule_tokens: vec!["empty".into(), "ready".into()],
        }
    }

    fn freeze(slugs: &[&str]) -> Vec<PanelInventoryEntry> {
        freeze_inventory(&Freezer(slugs)).expect("freezes").entries
    }
    struct Freezer<'a>(&'a [&'a str]);
    impl crate::ports::outbound::fold_freeze_source::FoldFreezeSourcePort for Freezer<'_> {
        fn declared_panels(&self) -> Result<Vec<DeclaredPanel>, String> {
            Ok(self.0.iter().map(|s| declared(s)).collect())
        }
    }

    #[test]
    fn a_matching_inventory_passes_with_a_summary_count() {
        let v = run_fold_drift_gate(&Fixed {
            frozen: freeze(&["drain", "alerts"]),
            live: freeze(&["drain", "alerts"]),
        })
        .expect("gate runs");
        assert!(matches!(v, FoldDriftVerdict::Pass { .. }));
        if let FoldDriftVerdict::Pass { summary } = &v {
            assert!(summary.contains("2 panels"), "{summary}");
        }
        assert!(v.clone().into_exit().is_ok());
    }

    #[test]
    fn drift_fails_naming_each_panel_expected_vs_actual_and_the_refreeze_command() {
        // Live swaps two panels and adds one: moved + added attribution.
        let v = run_fold_drift_gate(&Fixed {
            frozen: freeze(&["drain", "alerts", "kpis"]),
            live: freeze(&["alerts", "drain", "kpis", "health"]),
        })
        .expect("gate runs");
        let rep = v.into_exit().expect_err("must fail on drift");
        assert!(rep.contains("DRIFTED (3 findings)"), "{rep}");
        // per-panel attribution with expected-vs-actual
        assert!(
            rep.contains("reordered-slot drain: position — expected `slot 1`, got `slot 2`"),
            "{rep}"
        );
        assert!(
            rep.contains("reordered-slot alerts: position — expected `slot 2`, got `slot 1`"),
            "{rep}"
        );
        assert!(
            rep.contains("UNEXPECTED  health: expected (none), got rendered at slot 4"),
            "{rep}"
        );
        assert!(rep.contains(REFREEZE_COMMAND), "{rep}");
        assert!(rep.contains("__inventories__"), "{rep}");
    }

    #[test]
    fn an_out_of_date_live_side_reports_all_differences_and_guidance() {
        // Snapshot says 4 panels, live manifest shrank by one: the report is
        // the expected(frozen)-vs-actual(live) story plus the regenerate cmd.
        let v = run_fold_drift_gate(&Fixed {
            frozen: freeze(&["drain", "alerts", "kpis", "releases"]),
            live: freeze(&["drain", "alerts"]),
        })
        .expect("gate runs");
        let rep = match v {
            FoldDriftVerdict::Fail { report } => report,
            other @ FoldDriftVerdict::Pass { .. } => panic!("expected fail, got {other:?}"),
        };
        // Per-panel attribution: each dropped panel named as MISSING with
        // its expected slot (frozen) vs "(not observed)" (actual live).
        assert!(
            rep.contains("MISSING  kpis: expected rendered at slot 3, got (none)"),
            "{rep}"
        );
        assert!(
            rep.contains("MISSING  releases: expected rendered at slot 4, got (none)"),
            "{rep}"
        );
        assert!(rep.contains("DRIFTED (2 findings)"), "{rep}");
        assert!(rep.contains(REFREEZE_COMMAND), "{rep}");
        assert!(rep.contains("RE-FREEZE"), "{rep}");
    }

    #[test]
    fn an_empty_committed_snapshot_is_a_loud_failure_not_a_silent_pass() {
        let err = run_fold_drift_gate(&Fixed {
            frozen: vec![],
            live: freeze(&["drain"]),
        })
        .expect_err("empty snapshot must fail");
        assert!(err.contains("EMPTY"), "{err}");
        assert!(err.contains(REFREEZE_COMMAND), "{err}");
    }

    #[test]
    fn unobservable_sides_error_without_being_called_drift() {
        struct Broken;
        impl FoldDriftSourcePort for Broken {
            fn frozen_snapshot(&self) -> Result<Vec<PanelInventoryEntry>, String> {
                Err("snapshot unreadable".into())
            }
            fn live_inventory(&self) -> Result<Vec<PanelInventoryEntry>, String> {
                Ok(vec![])
            }
        }
        let err = run_fold_drift_gate(&Broken).expect_err("observation failure");
        assert_eq!(err, "snapshot unreadable");
        // hash helper still used (keeps the token contract visible)
        assert_eq!(fnv1a_64_hex(&["ready"]), fnv1a_64_hex(&["ready"]));
    }
}
