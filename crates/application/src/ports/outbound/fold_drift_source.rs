//! `FoldDriftSourcePort` — the boundary the drift guardrail (CXA-B302) reads
//! BOTH sides of the comparison through.
//!
//! One honest door for the whole check: the FROZEN snapshot arrives from the
//! committed file (`__inventories__/above_the_fold_panels.golden`), the LIVE
//! inventory is recomputed from the typed panel manifest at gate time. All
//! file/manifest IO lives in adapters; the gate itself stays a pure function
//! over what this port hands back, so tests inject doubles and never touch
//! disk.

use coxagent_domain::PanelInventoryEntry;

/// Outbound port: the two inventories the drift guardrail diffs.
///
/// Implemented by the app-layer composition (freeze-side adapter over
/// [`crate::ports::outbound::FoldFreezeSourcePort`] plus a snapshot-file
/// reader); guardrail tests substitute in-memory doubles.
pub trait FoldDriftSourcePort: Send + Sync {
    /// The committed frozen snapshot, parsed into canonical entries.
    ///
    /// # Errors
    /// When the snapshot cannot be read or a data line fails
    /// [`PanelInventoryEntry::parse_canonical`] — an unreadable freeze must
    /// fail loudly, never silently pass.
    fn frozen_snapshot(&self) -> Result<Vec<PanelInventoryEntry>, String>;

    /// The freshly computed live inventory, same shape as the freeze.
    ///
    /// # Errors
    /// When the live side cannot be observed (empty manifest, unavailable
    /// declaration source).
    fn live_inventory(&self) -> Result<Vec<PanelInventoryEntry>, String>;
}
