//! The registered `fold_panel_inventory_matches_snapshot` guardrail case
//! (CXA-B240.2): recompute the live above-the-fold inventory through the
//! outbound port and pure-diff it against the frozen snapshot.
//!
//! Registration only — all comparison logic is pure, in
//! [`crate::guardrails::fold_inventory`]; acquisition goes through
//! [`OverviewInventoryPort`] so tests substitute doubles and nothing here
//! touches fs/process.

use crate::ports::outbound::{LivePanelInventory, OverviewInventoryPort};
use crate::guardrails::fold_inventory::{empty_live_inventory_guidance, fold_inventory_diff};

/// Run the drift case against a live-inventory provider.
///
/// # Errors
/// Names the observation failure (unavailable port, empty live view) or the
/// rendered inventory drift so an unreadable live view can never count as a
/// silent pass; `Ok` lines confirm the freeze matched.
pub fn check_fold_inventory(port: &dyn OverviewInventoryPort) -> Result<Vec<String>, String> {
    let live: LivePanelInventory = match port.live_inventory() {
        Ok(inv) => inv,
        Err(e) => return Err(format!("live inventory unavailable: {e}")),
    };

    if live.panels.is_empty() {
        // Explicit remediation guidance, not a silent pass.
        return Err(empty_live_inventory_guidance(&live.viewport));
    }

    let frozen = frozen_panel_ids();
    let diff = fold_inventory_diff(&live.viewport, &frozen, &live.ordered_ids());
    if diff.is_clean() {
        Ok(vec![format!(
            "above-the-fold inventory matches the freeze: {} panels at viewport {}",
            diff.frozen_count, diff.viewport
        )])
    } else {
        Err(diff.render())
    }
}

/// The frozen sequence, embedded from the committed snapshot's data (kept in
/// lockstep by `tests/fold_inventory_guardrail_b281_tdd.rs`, which parses the
/// real snapshot file and compares it against this list).
#[must_use]
pub fn frozen_panel_ids() -> Vec<&'static str> {
    vec![
        "drain",
        "alerts",
        "working",
        "kpis",
        "health",
        "recent-activity",
        "releases",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::LivePanel;
    use std::sync::Mutex;

    struct Fixed(Option<LivePanelInventory>);
    impl OverviewInventoryPort for Fixed {
        fn live_inventory(&self) -> Result<LivePanelInventory, String> {
            self.0.clone().ok_or_else(|| "no observation".into())
        }
    }

    /// Records whether the double was consulted at all (the AC's "test double,
    /// never mutating prod code" requirement, made observable).
    #[allow(dead_code)]
    struct Spying(Mutex<usize>, Vec<LivePanel>);
    impl OverviewInventoryPort for Spying {
        fn live_inventory(&self) -> Result<LivePanelInventory, String> {
            *self.0.lock().unwrap() += 1;
            Ok(LivePanelInventory::new("desktop", self.1.clone()))
        }
    }

    fn p(id: &str) -> LivePanel {
        LivePanel::new(id, format!("ov-{id}"))
    }

    fn full() -> Vec<LivePanel> {
        ["drain", "alerts", "working", "kpis", "health"]
            .iter()
            .map(|s| p(s))
            .chain([LivePanel::new("recent-activity", "ov-activity"), p("releases")])
            .collect()
    }

    #[test]
    fn equal_inventories_pass() {
        let r = check_fold_inventory(&Fixed(Some(LivePanelInventory::new("desktop", full()))));
        assert!(r.is_ok());
        assert!(r.unwrap()[0].contains("matches the freeze"));
    }

    #[test]
    fn added_panel_fails_naming_it() {
        let mut panels = full();
        panels.insert(3, p("surprise-widget"));
        let r = check_fold_inventory(&Fixed(Some(LivePanelInventory::new("desktop", panels))));
        let msg = r.unwrap_err();
        assert!(msg.contains("`surprise-widget` added"), "{msg}");
    }

    #[test]
    fn removed_panel_fails_naming_it() {
        let mut panels = full();
        panels.retain(|x| x.id != "kpis");
        let r = check_fold_inventory(&Fixed(Some(LivePanelInventory::new("desktop", panels))));
        let msg = r.unwrap_err();
        assert!(msg.contains("`kpis` removed"), "{msg}");
    }

    #[test]
    fn moved_panel_fails_with_from_to_positions() {
        let mut panels = full();
        panels.swap(0, 1); // drain <-> alerts
        let r = check_fold_inventory(&Fixed(Some(LivePanelInventory::new("desktop", panels))));
        let msg = r.unwrap_err();
        assert!(msg.contains("`drain` moved from position 1 to 2"), "{msg}");
        assert!(msg.contains("`alerts` moved from position 2 to 1"), "{msg}");
    }

    #[test]
    fn empty_live_view_is_an_error_with_remediation_not_a_pass() {
        let r = check_fold_inventory(&Fixed(Some(LivePanelInventory::new("desktop", vec![]))));
        let msg = r.unwrap_err();
        assert!(msg.contains("observation failure, not a pass"), "{msg}");
    }

    #[test]
    fn port_error_surfaces_instead_of_silent_green() {
        let r = check_fold_inventory(&Fixed(None));
        assert_eq!(r.unwrap_err(), "live inventory unavailable: no observation");
    }

    #[test]
    fn spy_double_confirms_the_port_was_consulted_once() {
        let s = Spying(Mutex::new(0), full());
        let _ = check_fold_inventory(&s);
        assert_eq!(*s.0.lock().unwrap(), 1);
    }
}
