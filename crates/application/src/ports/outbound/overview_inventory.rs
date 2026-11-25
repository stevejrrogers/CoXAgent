//! `OverviewInventoryPort` — the boundary over the *live* above-the-fold
//! panel inventory of the Overview screen (CXA-B240.2).
//!
//! The frozen-inventory guardrail must compare what the layout actually
//! renders today against a committed snapshot. Reading that live list is IO
//! over presentation assets, so it lives behind this port: the application
//! layer defines the shape (`LivePanelInventory`), an infrastructure adapter
//! resolves it from the real source, and tests substitute their own double.
//! No `fs`/`process` access ever reaches domain or application code.

/// One live panel as the renderer exposes it: stable identifier plus the DOM
/// slot it paints into, in render order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePanel {
    /// Stable panel id, matching the domain registry's slug form
    /// (`drain`, `alerts`, … , `releases`).
    pub id: String,
    /// The DOM element id the panel paints into (`ov-drain`, …).
    pub dom_id: String,
}

impl LivePanel {
    pub fn new(id: impl Into<String>, dom_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            dom_id: dom_id.into(),
        }
    }
}

/// The ordered live inventory as observed by an adapter — render-order list of
/// [`LivePanel`]s plus the breakpoint/viewport label the observation was made
/// at (this codebase renders one desktop viewport, pinned here for drift
/// attribution rather than invented per-breakpoint rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePanelInventory {
    /// Viewport the inventory applies to (`"desktop"` — the only one rendered).
    pub viewport: String,
    /// Panels above the fold, in paint order.
    pub panels: Vec<LivePanel>,
}

impl LivePanelInventory {
    pub fn new(viewport: impl Into<String>, panels: Vec<LivePanel>) -> Self {
        Self {
            viewport: viewport.into(),
            panels,
        }
    }

    /// Just the ids, in order — the sequence the pure diff consumes.
    #[must_use]
    pub fn ordered_ids(&self) -> Vec<&str> {
        self.panels.iter().map(|p| p.id.as_str()).collect()
    }
}

/// Outbound port: acquire the live above-the-fold panel inventory.
///
/// Implemented by the infrastructure adapter that reads the real layout
/// source; guardrail tests inject a stub returning whatever scenario they
/// exercise. Fallible because the underlying read can fail — a caller turns
/// an error into an explicit drift-report failure, never a silent pass.
pub trait OverviewInventoryPort: Send + Sync {
    /// The live inventory observation, or why none could be taken.
    ///
    /// # Errors
    /// When no live observation can be taken (unreadable source, missing
    /// layout); the caller turns the error into an explicit drift-report
    /// failure rather than a silent pass.
    fn live_inventory(&self) -> Result<LivePanelInventory, String>;
}
