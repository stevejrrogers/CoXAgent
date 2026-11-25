//! `overview_contracts` (CXA-B256a, subtask 1/3 of CXA-B256) — the read the
//! Overview view/view-model will consume to get ALREADY-MAPPED panel data.
//!
//! Thin composition only: the identity mapping itself lives in the domain
//! (`coxagent_domain::overview_panel_contract`, CXA-B255); this use case walks
//! the panels a caller says are present, resolves each through that pure
//! mapping, and carries back the counts and empty-state metadata the panel
//! contract requires (zero state is data, not UI — the same rule
//! `domain::panel::PanelListing` already encodes).
//!
//! Purity: everything here is a function over existing domain/port value
//! types. No IO, no framework imports, no fixture invention — the caller
//! supplies panel presence (`OverviewPanelId`s) and gets back presentation-
//! facing rows plus attribution-grade totals.

use coxagent_domain::overview_panel::OverviewPanelId;
use coxagent_domain::overview_panel_contract::{panel_contract, PanelContract};

/// One presentation-facing row: a panel's resolved contract, ready for the
/// view-mesh. Cloned out of the domain lookup rather than borrowed so the
/// result owns its data (returning `&'static` would couple callers to the
/// const table forever).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverviewContractRow {
    /// Stable panel identity (enum variant, not the DOM id).
    pub id: OverviewPanelId,
    /// The DOM element the panel paints into (`ov-drain`, …), as mapped.
    pub dom_id: &'static str,
    /// The paint site the enumeration cited for this panel, as mapped.
    pub paint_site: &'static str,
}

impl From<&PanelContract> for OverviewContractRow {
    fn from(c: &PanelContract) -> Self {
        Self {
            id: c.id,
            dom_id: c.dom_id,
            paint_site: c.paint_site,
        }
    }
}

/// The mapped overview structure the presentation consumes: one row per
/// present panel (render order preserved), the matched total, the full
/// registry size distinguishing "nothing present" from "panels dropped by
/// the caller", and the empty-state hint when nothing is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewContracts {
    /// Mapped rows, in the order given (callers pass `RENDER_ORDER` unless
    /// deliberately filtering; the use case never reorders silently).
    pub rows: Vec<OverviewContractRow>,
    /// How many panels were present in this read (the list's total).
    pub total: usize,
    /// How many panels the screen defines at all — distinguishes "the source
    /// is empty" from "a subset was requested".
    pub registry_size: usize,
}

impl OverviewContracts {
    /// The defined empty result: zero state is data, not UI.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            rows: Vec::new(),
            total: 0,
            registry_size: 0,
        }
    }

    /// WHY the overview is empty, or `None` when it isn't — an operator-facing
    /// hint, not a bare blank panel.
    #[must_use]
    pub fn empty_state_hint(&self) -> Option<String> {
        if self.total > 0 || self.registry_size == 0 {
            return None;
        }
        Some(format!(
            "no above-the-fold panels are present in this read \
             (the screen defines {slots} slots) — panels appear once their \
             source inventory lists them",
            slots = OverviewPanelId::RENDER_ORDER.len()
        ))
    }
}

/// Resolve the mapped overview contracts for the panels present in `present`.
///
/// Presence semantics follow `guardrails::fold_inventory`: what the caller
/// hands in IS the observation; this use case adds no reads of its own. A
/// panel whose id is absent yields no row — counts reflect exactly what was
/// asked about, so a partial presence is visible in `total` vs
/// `registry_size` instead of being papered over.
#[must_use = "an unmapped overview must not be silently discarded"]
pub fn overview_contracts(present: &[OverviewPanelId]) -> OverviewContracts {
    let rows: Vec<OverviewContractRow> = present
        .iter()
        .map(|id| OverviewContractRow::from(&panel_contract(*id)))
        .collect();
    let total = rows.len();
    OverviewContracts {
        rows,
        total,
        registry_size: CONTRACT_SIZE,
    }
}

/// Convenience covering the canonical read: every panel, in declared render
/// order. Equivalent to `overview_contracts(OverviewPanelId::RENDER_ORDER)`.
#[must_use]
pub fn all_overview_contracts() -> OverviewContracts {
    overview_contracts(OverviewPanelId::RENDER_ORDER)
}

/// Size of the frozen enumeration, restated through the const-table length so
/// the two cannot drift independently (the domain tests already pin table ==
/// enum equality).
const CONTRACT_SIZE: usize = OverviewPanelId::RENDER_ORDER.len();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_render_order_maps_every_slot_with_counts_matching_the_registry() {
        let r = all_overview_contracts();
        assert_eq!(r.rows.len(), OverviewPanelId::RENDER_ORDER.len());
        assert_eq!(r.total, r.rows.len());
        assert_eq!(r.registry_size, r.rows.len());
        // Order is the enumeration's render order, one-to-one, no reuse.
        for (row, id) in r.rows.iter().zip(OverviewPanelId::RENDER_ORDER) {
            assert_eq!(row.id, *id);
        }
    }

    #[test]
    fn a_mapped_row_carries_the_enumeration_dom_id_and_paint_site() {
        let drain = panel_contract(OverviewPanelId::OvDrain);
        let kpis = panel_contract(OverviewPanelId::Kpis);
        assert_eq!(kpis.dom_id, "kpis");
        assert_eq!(drain.dom_id, "ov-drain");
        assert!(!drain.paint_site.is_empty() && !kpis.paint_site.is_empty());
        let r = overview_contracts(&[OverviewPanelId::OvDrain]);
        assert_eq!(r.rows[0].dom_id, drain.dom_id);
        assert_eq!(r.rows[0].paint_site, drain.paint_site);
    }

    #[test]
    fn empty_source_data_is_an_explicit_empty_state_not_a_bare_zero() {
        let r = overview_contracts(&[]);
        // Empty SOURCE, not an empty SCREEN: no rows, zero matched total,
        // yet the full registry size is still reported so a consumer can
        // tell "panels exist but none are present" from "no panels at all".
        assert!(r.rows.is_empty());
        assert_eq!(r.total, 0);
        assert_eq!(r.registry_size, OverviewPanelId::RENDER_ORDER.len());
        assert_ne!(r, OverviewContracts::empty());
        let hint = r.empty_state_hint().expect("empty read names why");
        assert!(hint.contains("no above-the-fold panels"), "{hint}");
        assert!(hint.contains("source inventory"), "{hint}");
    }

    #[test]
    fn a_nonempty_read_has_no_empty_state_hint_even_when_partial() {
        let r = overview_contracts(&[OverviewPanelId::OvAlerts]);
        assert_eq!(r.total, 1);
        // Partial presence: fewer rows than the registry holds, but not empty.
        assert!(r.rows.len() < r.registry_size);
        assert!(r.empty_state_hint().is_none());
    }

    #[test]
    fn counts_distinguish_matched_from_registry_total() {
        let three = [
            OverviewPanelId::OvWorking,
            OverviewPanelId::OvHealth,
            OverviewPanelId::OvChangelog,
        ];
        let r = overview_contracts(&three);
        assert_eq!(r.total, 3);
        assert_eq!(r.registry_size, OverviewPanelId::RENDER_ORDER.len());
        assert_ne!(r.total, r.registry_size);
    }
}
