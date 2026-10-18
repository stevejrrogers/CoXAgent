//! Above-the-fold Overview panels — the exhaustive, evidence-backed
//! enumeration (CXA-B228, subtask 1/3 of the CXA-B195 lineage).
//!
//! Ground truth is the shipped Overview shell,
//! `crates/presentation/src/web/index.html` (`#view-overview`, lines 202–227).
//! "Above the fold" is the span before the `<details class="ov-diag">`
//! diagnostics drawer (line 214) — the CXA-F383 declutter moved everything
//! diagnostic below that boundary, and the in-file comment at lines 203–204
//! states the same rule.
//!
//! This module is pure: types and constants only, zero IO, zero framework
//! imports. The accompanying unit test
//! (`crates/domain/tests/overview_panel_render_order_b228.rs`) pins the
//! declared render order to the enum's case order.

/// One above-the-fold panel of the Overview screen, in DOM/render order.
///
/// Each case is named after the stable DOM id of the container it renders
/// into (`index.html`, `#view-overview`), and its doc comment cites the
/// rendering evidence (file:line of the DOM mount + the JS paint site).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverviewPanelId {
    /// Clean-base drain banner — `<div id="ov-drain">`
    /// (crates/presentation/src/web/index.html:205); painted by
    /// `drainBanner("ov-drain")` (crates/presentation/src/web/js/core.js:797,
    /// shell.js:750).
    OvDrain,
    /// Operator alert strip — `<div id="ov-alerts">`
    /// (crates/presentation/src/web/index.html:206); painted by
    /// `alertsHtml` (crates/presentation/src/web/js/core.js:796).
    OvAlerts,
    /// Busy-agent chips — `<div id="ov-working">`
    /// (crates/presentation/src/web/index.html:207); painted by the workers
    /// renderer (crates/presentation/src/web/js/core.js:1022).
    OvWorking,
    /// KPI tile strip — `<div class="kpis" id="kpis">`
    /// (crates/presentation/src/web/index.html:208); painted by
    /// `overviewKpis` (crates/presentation/src/web/js/core.js:798,
    /// kpis.js:76).
    Kpis,
    /// Team-health cards — `<div id="ov-health">`
    /// (crates/presentation/src/web/index.html:209); painted by the health
    /// renderer (crates/presentation/src/web/js/chat.js:1713).
    OvHealth,
    /// Recent-activity feed — `<div id="ov-activity">`
    /// (crates/presentation/src/web/index.html:211, inside the left
    /// `.panel` of the `row2` split); painted by the activity renderer
    /// (crates/presentation/src/web/js/core.js:814).
    OvActivity,
    /// Releases feed — `<div id="ov-changelog">`
    /// (crates/presentation/src/web/index.html:212, right `.panel` of the
    /// `row2` split); painted by the releases renderer
    /// (crates/presentation/src/web/js/core.js:816).
    OvChangelog,
}

impl OverviewPanelId {
    /// The panel's stable DOM id (the `id=` attribute it renders into).
    #[must_use]
    pub const fn dom_id(self) -> &'static str {
        match self {
            Self::OvDrain => "ov-drain",
            Self::OvAlerts => "ov-alerts",
            Self::OvWorking => "ov-working",
            Self::Kpis => "kpis",
            Self::OvHealth => "ov-health",
            Self::OvActivity => "ov-activity",
            Self::OvChangelog => "ov-changelog",
        }
    }

    /// Every above-the-fold panel, top-of-view to bottom — the render order
    /// of `#view-overview` in `crates/presentation/src/web/index.html`
    /// (lines 205–212). Exposed as a slice so consumers can iterate without
    /// depending on `strum`/variant-order tricks.
    pub const RENDER_ORDER: &'static [Self] = &[
        Self::OvDrain,
        Self::OvAlerts,
        Self::OvWorking,
        Self::Kpis,
        Self::OvHealth,
        Self::OvActivity,
        Self::OvChangelog,
    ];
}

#[cfg(test)]
mod tests {
    use super::OverviewPanelId;

    /// The declared render order must equal the enum's case order: the
    /// enumeration *is* the contract, so a re-ordering or an inserted case
    /// must be a conscious edit of both together (CXA-B228 AC3).
    #[test]
    fn declared_render_order_matches_the_enum_case_order() {
        let cases: &[OverviewPanelId] = &[
            OverviewPanelId::OvDrain,
            OverviewPanelId::OvAlerts,
            OverviewPanelId::OvWorking,
            OverviewPanelId::Kpis,
            OverviewPanelId::OvHealth,
            OverviewPanelId::OvActivity,
            OverviewPanelId::OvChangelog,
        ];
        assert_eq!(OverviewPanelId::RENDER_ORDER, cases);
    }

    /// Every case's dom_id is unique — the enumeration is exhaustive over
    /// distinct mounts, not two spellings of one panel.
    #[test]
    fn dom_ids_are_unique() {
        let ids: Vec<_> = OverviewPanelId::RENDER_ORDER
            .iter()
            .map(|p| p.dom_id())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len());
    }
}
