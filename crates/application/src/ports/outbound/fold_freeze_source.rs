//! `FoldFreezeSourcePort` — the boundary for FREEZING the above-the-fold
//! panel inventory (CXA-B281.a).
//!
//! `OverviewInventoryPort` answers "what does the layout render right now?"
//! for drift checks. Freezing needs *declared* facts instead — stable slug,
//! owning component, declared visibility-rule tokens — because the snapshot
//! must be reproducible from the source of truth, not from a moment in time.
//! Those declarations live in presentation assets, so they arrive through
//! this port and the use case stays a pure assembly over the returned rows.

/// One declared panel row, exactly what the freeze needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredPanel {
    /// Stable registry slug (`drain`, `recent-activity`, …) — becomes
    /// `panel_id` in the frozen record.
    pub slug: String,
    /// The DOM slot the panel paints into (`ov-drain`, …).
    pub dom_id: String,
    /// The client module/function owning fetch + paint
    /// (`core.js alertsHtml()`) — becomes `component_name`.
    pub component: String,
    /// The declared visibility/terminal-state rule token names, IN CONTRACT
    /// ORDER (the hash is deliberately order-sensitive).
    pub visibility_rule_tokens: Vec<String>,
}

/// Outbound port: the DECLARED above-the-fold panels, top-of-view to bottom.
///
/// Implemented by the presentation adapter over the typed panel manifest;
/// guardrail/freezer tests substitute doubles.
pub trait FoldFreezeSourcePort: Send + Sync {
    /// The declared rows in render order, or why none could be read.
    ///
    /// # Errors
    /// When the declarations cannot be produced (empty/unavailable manifest);
    /// the caller surfaces the reason rather than freezing an empty document.
    fn declared_panels(&self) -> Result<Vec<DeclaredPanel>, String>;
}
