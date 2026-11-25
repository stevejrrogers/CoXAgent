//! The above-the-fold panel inventory guardrail (CXA-B240.2) — PURE diff
//! between the frozen snapshot and a live observation.
//!
//! The layout enumeration itself lives in presentation; here we only compare
//! sequences. `fold_inventory_diff` is a total function over two ordered id
//! lists: same sequence → no drift; anything else names each affected panel
//! with its direction (`added` / `removed` / `moved from→to`) so a failing CI
//! line reads like a sentence. Acquiring the *live* side goes through
//! [`crate::ports::outbound::OverviewInventoryPort`]; this module never does IO.

use std::fmt::Write as _;

/// One drift finding: which panel, which way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelDrift {
    /// Present live, absent from the frozen snapshot.
    Added { panel: String },
    /// Frozen but not rendered anymore.
    Removed { panel: String },
    /// Still present at both ends but at a new position.
    Moved { panel: String, from: usize, to: usize },
}

impl PanelDrift {
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Added { panel } => format!("panel `{panel}` added"),
            Self::Removed { panel } => format!("panel `{panel}` removed"),
            Self::Moved { panel, from, to } => {
                format!("panel `{panel}` moved from position {from} to {to}")
            }
        }
    }
}

/// The full comparison result: findings plus the counts the report must show
/// (frozen panel count vs above-fold/live panel count).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldInventoryDiff {
    pub viewport: String,
    pub frozen_count: usize,
    pub live_count: usize,
    pub drifts: Vec<PanelDrift>,
}

impl FoldInventoryDiff {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.drifts.is_empty()
    }

    /// One human-readable summary of all findings, empty when clean.
    #[must_use]
    pub fn render(&self) -> String {
        if self.is_clean() {
            return String::new();
        }
        let mut out = String::new();
        let _ = writeln!(
            out,
            "above-the-fold panel inventory drifted (viewport {}): frozen {} panels, live {}",
            self.viewport, self.frozen_count, self.live_count
        );
        for d in &self.drifts {
            let _ = writeln!(out, "  - {}", d.describe());
        }
        out.trim_end().to_owned()
    }
}

/// Pure diff of two ordered panel-id sequences. Same set, same order → clean;
/// otherwise every panel that appeared, disappeared or changed position gets
/// exactly one named finding.
#[must_use]
pub fn fold_inventory_diff(
    viewport: &str,
    frozen: &[&str],
    live: &[&str],
) -> FoldInventoryDiff {
    let mut drifts = Vec::new();

    // Moves are judged on RELATIVE order of the panels both sides agree on:
    // an add/remove elsewhere must not read as every neighbour "moving".
    let common: Vec<&str> = frozen
        .iter()
        .copied()
        .filter(|p| live.contains(p))
        .collect();
    let same_relative_order = live
        .iter()
        .copied()
        .filter(|p| frozen.contains(p))
        .eq(common.iter().copied());

    if !same_relative_order {
        // Only panels BOTH sides still show can be judged as "moved": report
        // the ones whose relative standing changed, at their absolute slots.
        let rank = |seq: &[&str], panel: &str| seq.iter().position(|p| *p == panel);
        for panel in &common {
            if let Some(from) = rank(frozen, panel) {
                if let Some(to) = rank(live, panel) {
                    if from != to {
                        drifts.push(PanelDrift::Moved {
                            panel: (*panel).to_owned(),
                            from: from + 1,
                            to: to + 1,
                        });
                    }
                }
            }
        }
    }

    // Removals: frozen but not rendered anymore.
    for panel in frozen {
        if !live.contains(panel) {
            drifts.push(PanelDrift::Removed {
                panel: (*panel).to_owned(),
            });
        }
    }
    // Additions: live entries the freeze has never heard of.
    for panel in live {
        if !frozen.contains(panel) {
            drifts.push(PanelDrift::Added {
                panel: (*panel).to_owned(),
            });
        }
    }

    FoldInventoryDiff {
        viewport: viewport.to_owned(),
        frozen_count: frozen.len(),
        live_count: live.len(),
        drifts,
    }
}

/// Explicit remediation text for an EMPTY live observation. An adapter that
/// cannot see any panels is an observation failure, never a passing check:
/// the guidance says what to look at instead of silently handing back green.
#[must_use]
pub fn empty_live_inventory_guidance(viewport: &str) -> String {
    format!(
        "live above-the-fold inventory is EMPTY (no panels observed at viewport {viewport}); \
         this is an observation failure, not a pass — check the OverviewInventoryPort adapter \
         wiring and the index.html above-the-fold section before trusting this run"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROZEN: [&str; 5] = ["drain", "alerts", "kpis", "health", "releases"];

    #[test]
    fn equal_sequences_are_clean_with_matching_counts() {
        let d = fold_inventory_diff("desktop", &FROZEN, &["drain", "alerts", "kpis", "health", "releases"]);
        assert!(d.is_clean());
        assert_eq!((d.frozen_count, d.live_count), (5, 5));
    }

    #[test]
    fn an_addition_names_the_panel_and_direction() {
        let mut live: Vec<&str> = FROZEN.to_vec();
        live.insert(2, "new-thing");
        let d = fold_inventory_diff("desktop", &FROZEN, &live);
        assert_eq!(d.drifts, vec![PanelDrift::Added { panel: "new-thing".into() }]);
        assert!(d.render().contains("panel `new-thing` added"));
    }

    #[test]
    fn a_removal_names_the_panel_and_direction() {
        let live: Vec<&str> = FROZEN[1..].to_vec();
        let d = fold_inventory_diff("desktop", &FROZEN, &live);
        assert_eq!(d.drifts, vec![PanelDrift::Removed { panel: "drain".into() }]);
        assert!(d.render().contains("panel `drain` removed"));
    }

    #[test]
    fn a_reorder_reports_moved_from_to_positions() {
        let live = ["alerts", "drain", "kpis", "health", "releases"];
        let d = fold_inventory_diff("desktop", &FROZEN, &live);
        assert_eq!(
            d.drifts,
            vec![
                PanelDrift::Moved { panel: "drain".into(), from: 1, to: 2 },
                PanelDrift::Moved { panel: "alerts".into(), from: 2, to: 1 },
            ]
        );
        assert!(d.render().contains("panel `drain` moved from position 1 to 2"));
    }

    #[test]
    fn counts_reflect_both_sides_even_mid_drift() {
        let d = fold_inventory_diff("desktop", &FROZEN, &["drain", "extra", "alerts"]);
        assert_eq!((d.frozen_count, d.live_count), (5, 3));
        assert_eq!(d.drifts.len(), 4); // kpis/health/releases removed, extra added
    }

    #[test]
    fn an_empty_live_observation_gets_explicit_guidance_text() {
        let msg = empty_live_inventory_guidance("desktop");
        assert!(msg.contains("EMPTY"));
        assert!(msg.contains("observation failure, not a pass"));
        assert!(msg.contains("OverviewInventoryPort adapter"));
    }
}
