//! The Overview→panel-contract mapping (CXA-B255, subtask 1/3 of CXA-B232).
//!
//! ONE pure function, [`panel_contract`], maps an [`OverviewPanelId`] — the
//! frozen above-the-fold enumeration (CXA-B228,
//! [`crate::overview_panel`]) — to its [`PanelContract`]: the identity a
//! panel wiring implementation (B232 part 2/3) must satisfy. Later parts
//! compose the per-panel terminal states ([`crate::terminal_state`]) on top;
//! this module deliberately carries ONLY what part 2 needs to bind a panel
//! to its mount and its paint site.
//!
//! Ground truth, cited per variant like [`crate::overview_panel`] does, is
//! the shipped Overview shell (`crates/presentation/src/web/index.html`,
//! `#view-overview`) and the JS renderer that paints into each mount.
//!
//! Purity: std-only, zero IO, zero framework imports. The mapping is TOTAL
//! and INDEPENDENT OF ANY SNAPSHOT: even with an empty `/state` response
//! every panel resolves to its contract — there is deliberately no "no
//! contract" path, because panel identity is static screen structure, not
//! data. Exhaustiveness is compiler-enforced (one arm per
//! [`OverviewPanelId`] variant), and [`CONTRACTS`] is derived FROM that
//! function so the table and the mapping cannot drift apart; the in-module
//! tests pin both against [`OverviewPanelId::RENDER_ORDER`].

use crate::overview_panel::OverviewPanelId;

/// Where a panel gets painted — the stable binding part 2/3 wires against.
///
/// `dom_id` is the element the panel renders into; `paint_site` cites the
/// rendering code (file:line + symbol) that owns the paint today, as
/// evidence gathered during the CXA-B228 enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PanelContract {
    /// Stable panel identity — reference this, never the DOM id, in code.
    pub id: OverviewPanelId,
    /// The element id the panel paints into inside `view-overview`.
    pub dom_id: &'static str,
    /// Rendering evidence for this panel: the JS paint site, `file:line symbol`.
    pub paint_site: &'static str,
}

impl PanelContract {
    /// Construct a contract from the values every entry must cite.
    #[must_use]
    pub const fn new(id: OverviewPanelId, dom_id: &'static str, paint_site: &'static str) -> Self {
        Self {
            id,
            dom_id,
            paint_site,
        }
    }
}

/// The PURE Overview→contract mapping: total over
/// [`OverviewPanelId::RENDER_ORDER`], one match arm per variant (a panel
/// added without an arm here is a compile error, not a silently unwired
/// mount). Each arm restates the variant's own `dom_id()` and cites the same
/// render evidence the enumeration recorded; the tests keep the restatement
/// honest.
///
/// Note the mapping takes NO snapshot/state parameter on purpose: a panel's
/// contract exists even when the hub answers with nothing (the empty state)
/// — the terminal-state layer owns what an empty panel shows, this layer
/// owns what the panel IS.
#[must_use]
pub const fn panel_contract(id: OverviewPanelId) -> PanelContract {
    match id {
        OverviewPanelId::OvDrain => PanelContract::new(
            id,
            "ov-drain",
            "crates/presentation/src/web/js/core.js:797 drainBanner(\"ov-drain\") · shell.js:750",
        ),
        OverviewPanelId::OvAlerts => PanelContract::new(
            id,
            "ov-alerts",
            "crates/presentation/src/web/js/core.js:796 alertsHtml()",
        ),
        OverviewPanelId::OvWorking => PanelContract::new(
            id,
            "ov-working",
            "crates/presentation/src/web/js/core.js:1022 workers renderer",
        ),
        OverviewPanelId::Kpis => PanelContract::new(
            id,
            "kpis",
            "crates/presentation/src/web/js/core.js:798 overviewKpis() · kpis.js:76",
        ),
        OverviewPanelId::OvHealth => PanelContract::new(
            id,
            "ov-health",
            "crates/presentation/src/web/js/chat.js:1713 health renderer",
        ),
        OverviewPanelId::OvActivity => PanelContract::new(
            id,
            "ov-activity",
            "crates/presentation/src/web/js/core.js:814 activity renderer",
        ),
        OverviewPanelId::OvChangelog => PanelContract::new(
            id,
            "ov-changelog",
            "crates/presentation/src/web/js/core.js:816 releases renderer",
        ),
    }
}

/// Every above-the-fold panel's contract, in [`OverviewPanelId::RENDER_ORDER`]
/// — i.e. DOM order. Derived from [`panel_contract`] one-to-one, so the
/// table cannot disagree with the function by construction.
pub const CONTRACTS: &[PanelContract] = &[
    panel_contract(OverviewPanelId::OvDrain),
    panel_contract(OverviewPanelId::OvAlerts),
    panel_contract(OverviewPanelId::OvWorking),
    panel_contract(OverviewPanelId::Kpis),
    panel_contract(OverviewPanelId::OvHealth),
    panel_contract(OverviewPanelId::OvActivity),
    panel_contract(OverviewPanelId::OvChangelog),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// HAPPY PATH: a representative panel resolves to its exact contract —
    /// identity, mount and paint-site evidence all present and correct.
    #[test]
    fn the_kpis_panel_maps_to_its_contract() {
        let c = panel_contract(OverviewPanelId::Kpis);
        assert_eq!(c.id, OverviewPanelId::Kpis);
        assert_eq!(c.dom_id, "kpis");
        assert_eq!(
            c.paint_site,
            "crates/presentation/src/web/js/core.js:798 overviewKpis() · kpis.js:76"
        );
    }

    /// TOTALITY / EVERY BRANCH: the match must answer for ALL seven panels —
    /// each arm's dom_id equals the enumeration's own `dom_id()` and cites a
    /// non-blank paint site. This walks every conditional branch of the
    /// mapping, empty-snapshot-free by construction (see module docs).
    #[test]
    fn every_panel_in_render_order_resolves_to_a_contract() {
        for id in OverviewPanelId::RENDER_ORDER {
            let c = panel_contract(*id);
            assert_eq!(c.id, *id, "{id:?}: mapping returned another panel");
            assert_eq!(
                c.dom_id,
                id.dom_id(),
                "{id:?}: mapped dom_id drifted from the enumeration"
            );
            assert!(
                !c.paint_site.trim().is_empty(),
                "{id:?}: paint-site evidence missing"
            );
        }
    }

    /// The derived table stays in RENDER_ORDER (DOM order) and is one-to-one
    /// with the enumeration — same length, same ids, no duplicates.
    #[test]
    fn contracts_table_tracks_render_order_one_to_one() {
        let ids: Vec<_> = CONTRACTS.iter().map(|c| c.id).collect();
        let expected: Vec<_> = OverviewPanelId::RENDER_ORDER.to_vec();
        assert_eq!(ids, expected, "CONTRACTS must follow RENDER_ORDER");
        let mut sorted = ids.clone();
        sorted.sort_unstable_by_key(|id| id.dom_id());
        sorted.dedup_by_key(|id| id.dom_id());
        assert_eq!(sorted.len(), ids.len(), "duplicate panel in CONTRACTS");
    }

    /// Mounts are distinct across the whole screen: two panels sharing a
    /// dom_id would fight over one element.
    #[test]
    fn contract_dom_ids_are_unique() {
        let mut doms: Vec<&str> = CONTRACTS.iter().map(|c| c.dom_id).collect();
        doms.sort_unstable();
        doms.dedup();
        assert_eq!(doms.len(), CONTRACTS.len());
    }

    /// The empty-state rule made explicit: the mapping needs no snapshot —
    /// a contract built without any hub state answers identically to one
    /// built after data arrives, so a bare dashboard still knows WHAT each
    /// mount is (the terminal-state layer owns the EMPTY/ZERO copy).
    #[test]
    fn the_mapping_is_snapshot_free_and_stable() {
        let before = panel_contract(OverviewPanelId::OvDrain);
        // Call again; purity means identical value, no hidden state.
        let again = panel_contract(OverviewPanelId::OvDrain);
        assert_eq!(before, again);
        assert_eq!(before.dom_id, "ov-drain");
    }
}
