//! Sweep-diagnosis records (CXA-F404 [2/3], part 2 of CXA-F376) — the
//! persisted output of the daily sweep run.
//!
//! Application state, same tier as the [`AttemptFailure`] /
//! [`PrOpenHold`](crate::state::work::PrOpenHold) ledgers: plain data with
//! attribution on every record (`at` RFC3339, `by` actor) per the depth bar.
//! Additive `#[serde(default)]` fields — old documents load with empty maps,
//! older builds ignore the keys (the same zero-migration path PrOpenHold
//! took). No IO here; the sweep runner owns it.
//!
//! The decision itself stays single-sourced in part 1's pure core
//! ([`crate::use_cases::daily_recovery::recovery_eligibility`]) — this module
//! holds only the *persisted shape* of its verdicts.

use serde::{Deserialize, Serialize};

/// One stuck ticket's persisted diagnosis line, stamped when the daily sweep
/// decided its fate. Stored per ticket (latest wins, overwritten each day —
/// bounded by the held-ticket count, never append-only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepDiagnosis {
    /// Ticket id (per-project alias form, e.g. `CXC-B001`).
    pub ticket: String,
    /// UTC day key this diagnosis belongs to (`YYYY-MM-DD`) — makes
    /// "same diagnosis, same day" a no-op by construction.
    pub day: String,
    /// Whether the sweep resumed the ticket or kept it held.
    pub verdict: String,
    /// Why the ticket was auto-held in the first place.
    pub hold_reason: String,
    /// Full attribution/diagnosis sentence (the part-1 core's line).
    pub detail: String,
    /// When the sweep decided (RFC3339).
    pub at: String,
    /// Which actor wrote it — always `daily-sweep`.
    pub by: String,
}

impl SweepDiagnosis {
    /// Who writes every diagnosis record (single-sourced so tests and the
    /// panel agree).
    pub const ACTOR: &'static str = crate::use_cases::daily_recovery::SWEEP_ACTOR;
}

/// Phase of one UTC day's sweep run — the day ledger AND the once-per-day
/// idempotency lock (see `sweep_runner::claim_sweep_day`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SweepPhase {
    /// A runner claimed the day and is working through it. Older than 30
    /// minutes ⇒ the claiming runner crashed and another may reclaim.
    Running,
    /// The pass finished; the day is done and further runs are no-ops.
    Complete,
}

/// One UTC day's sweep-run ledger entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepDay {
    /// UTC day key (`YYYY-MM-DD`), the map key too.
    pub day: String,
    /// Running until the pass finishes, then Complete.
    pub phase: SweepPhase,
    /// How many held-bug candidates the sweep examined.
    pub subjects: usize,
    /// How many it resumed.
    pub recovered: usize,
    /// How many it left held (with a named cause each).
    pub escalated: usize,
    /// When this record was last written (RFC3339).
    pub at: String,
    /// Always `daily-sweep`; reclaims overwrite the original runner's entry.
    pub by: String,
    /// True when a crashed runner's stale `Running` entry was taken over.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reclaimed: bool,
    /// Zero-state hint for a day with nothing to do — silence must be
    /// distinguishable from "never ran".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
}
