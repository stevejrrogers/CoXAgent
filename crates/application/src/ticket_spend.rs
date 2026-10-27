//! Per-ticket engine-cost attribution panel (CXA-F377) — PURE core, zero IO.
//!
//! The money path already meters every engine run per role and folds it into
//! [`crate::state::Spend`] at end of cycle; F257 stamps which engine executed
//! each step. What was missing is a ticket-scoped budget line: "what did
//! CXA-F376 actually cost, against what estimate?" This module turns the
//! persisted per-ticket rollups (see `ProjectState.ticket_spend`) into the
//! flag decisions and the Fleet Spend top-N panel.
//!
//! Flag rule ("no line, no flag"), mirroring `cap_status(None)`:
//! - explicit `budget_usd` on the ticket when set;
//! - else the project's `approve_over_usd` inherited **at flag time**;
//! - neither ⇒ status `None`, never flagged.
//!
//! Status comes from the SAME threshold primitive fleet.rs uses
//! ([`crate::policy::approaching_cap`]) — single source for cap arithmetic.

use crate::fleet::CapStatus;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Default number of rows shown in the per-ticket panel (depth bar: windowed
/// list with counts; `?limit=` may raise it up to [`MAX_LIMIT`]).
pub const DEFAULT_LIMIT: usize = 25;
/// Hard ceiling for the panel window — keeps payloads bounded on big projects.
pub const MAX_LIMIT: usize = 100;

/// One ticket's actual engine-spend rollup, as persisted in
/// `ProjectState.ticket_spend` (snapshot input — pure data, no behaviour).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketSpendRollup {
    /// Total ACTUAL engine spend attributed to this ticket, USD.
    pub total_usd: f64,
    /// Spend split by executing engine (`engine -> usd`), e.g. the F370-class
    /// probe engines a retry loop burned before settling.
    pub by_engine: BTreeMap<String, f64>,
    /// Spend split by agent role.
    pub by_role: BTreeMap<String, f64>,
    /// Number of metered runs attributed here (a $0 run still counts — a
    /// zero-usage run is visible, not invisible).
    pub runs: u64,
    /// Runs that failed for engine/infra reasons AFTER metering partial cost
    /// — their spend is charged to infra, excluded from the ticket's work
    /// figure at presentation time but kept visible per AC3.
    #[serde(default)]
    pub infra_fault_runs: u64,
    /// Label stamped on the most recent contributing run (RFC3339); `None`
    /// only if the map entry exists but no timestamp survived the drain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
}

impl Default for TicketSpendRollup {
    fn default() -> Self {
        Self {
            total_usd: 0.0,
            by_engine: BTreeMap::new(),
            by_role: BTreeMap::new(),
            runs: 0,
            infra_fault_runs: 0,
            last_run_at: None,
        }
    }
}

impl TicketSpendRollup {
    /// Fold one metered run's charge into this rollup. Cost that arrived
    /// BEFORE an engine fault is folded too — a crash mid-flight must not
    /// silently drop spend already metered (AC5).
    pub fn fold(
        &mut self,
        cost_usd: f64,
        engine: Option<&str>,
        role: Option<&str>,
        ran_at: Option<&str>,
    ) {
        self.total_usd += cost_usd.max(0.0);
        *self
            .by_engine
            .entry(engine.unwrap_or("unknown").to_owned())
            .or_default() += cost_usd.max(0.0);
        *self.by_role.entry(role.unwrap_or("unknown").to_owned()).or_default() += cost_usd.max(0.0);
        self.runs += 1;
        // "later wins" is right because drains are chronological per label.
        if let Some(at) = ran_at {
            self.last_run_at = Some(at.to_owned());
        }
    }

    /// Runs charged to infrastructure rather than the ticket's work cost
    /// (engine/infra faults: revoked auth, quota wall, rate limit, network,
    /// provider death). Counted separately so they stay visible while being
    /// excluded from the work figure. Caller passes the infra-only share.
    pub fn fold_infra(&mut self, count: u64) {
        self.infra_fault_runs += count;
    }
}
/// Where a failed-engine charge landed (CXA-F370 contract). The presentation
/// layer derives the "charged to infra" chip from this, so both stay honest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeKind {
    /// Productive (or attemptable) work run — bill the ticket.
    Work,
    /// Engine/infra-side failure — bill nothing to the ticket's work figure,
    /// show "charged to infra" and keep the run count.
    InfraFault,
}

/// A single row of the per-ticket panel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TicketSpendRow {
    pub ticket_id: String,
    /// Snapshot input copied through for the UI's drill-down link.
    pub title: Option<String>,
    /// Actual spend charged to the ticket's WORK (infra-fault charges sit in
    /// `infra_fault_usd` beside it, never inside this figure).
    pub total_usd: f64,
    pub runs: u64,
    /// Runs failed for engine/infra reasons — excluded above, still counted.
    pub infra_fault_runs: u64,
    pub by_engine: BTreeMap<String, f64>,
    pub by_role: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// The budget line applied at flag time (explicit or inherited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    /// Whether the budget came from the ticket itself or project inheritance.
    #[serde(default, skip_serializing_if = "std::option::Option::is_none")]
    pub budget_source: Option<&'static str>,
    /// Over-budget multiplier vs the flag line (e.g. 3.1 for "3.1×"); `None`
    /// when there is no line or spend is under it.
    #[serde(default, skip_serializing_if = "std::option::Option::is_none")]
    pub over_multiplier: Option<f64>,
    pub status: CapStatus,
}

/// Panel totals + top-N rows, sorted cost-desc with id tie-break (mirrors the
/// fleet row sort so operators see a stable order across windows).
#[derive(Debug, Clone, Serialize)]
pub struct TicketSpendPanel {
    pub rows: Vec<TicketSpendRow>,
    pub total_rows: usize,
    pub total_usd: f64,
    pub total_runs: u64,
    pub over_count: usize,
    pub approaching_count: usize,
    /// Tickets whose runs carry NO ticket label at all — flagged, never
    /// dropped (mirror of broken-project handling): their cost lands here and
    /// in the unattributed bucket on state, outside any ticket row.
    pub unattributed: TicketSpendUnattributedSummary,
    /// Window actually applied after clamping to [`MAX_LIMIT`].
    pub limit: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TicketSpendUnattributedSummary {
    pub total_usd: f64,
    pub runs: u64,
}

/// Decide a ticket's flag line WITHOUT re-derived arithmetic:
/// explicit ticket budget first, else project `approve_over_usd`.
///
/// Returns `(line, source)`; `None` when neither exists ⇒ "no line, no flag".
#[must_use]
pub fn ticket_budget_line(
    budget_usd: Option<f64>,
    approve_over_usd: Option<f64>,
) -> (Option<f64>, Option<&'static str>) {
    match (budget_usd.filter(|v| *v > 0.0), approve_over_usd) {
        (Some(v), _) => (Some(v), Some("ticket")),
        (None, Some(v)) if v > 0.0 => (Some(v), Some("project")),
        _ => (None, None),
    }
}

/// Status of one ticket against its effective line, via the shared
/// `policy::approaching_cap` primitive — same thresholds as every other cap.
#[must_use]
pub fn ticket_budget_status(total_usd: f64, budget: Option<f64>, warn_pct: f64) -> CapStatus {
    if budget.is_some_and(|b| b > 0.0 && total_usd >= b) {
        return CapStatus::Over;
    }
    if crate::policy::approaching_cap(total_usd, budget, warn_pct) {
        return CapStatus::Approaching;
    }
    CapStatus::Ok
}

/// Build the panel. `unattributed_usd/runs` feed the always-visible bucket of
/// runs that had no ticket label; `titles` maps ids to titles for drill-down.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_ticket_spend_panel(
    rollups: &BTreeMap<String, TicketSpendRollup>,
    budgets: &BTreeMap<String, Option<f64>>,
    approve_over_usd: Option<f64>,
    titles: &BTreeMap<String, String>,
    unattributed: TicketSpendRollup,
    warn_pct: f64,
    limit: usize,
) -> TicketSpendPanel {
    let mut rows: Vec<TicketSpendRow> = rollups
        .iter()
        .map(|(id, r)| {
            let (budget, source) = {
                let explicit = budgets.get(id).copied().flatten();
                let (line, src) = ticket_budget_line(explicit, approve_over_usd);
                (line, src)
            };
            let status = ticket_budget_status(r.total_usd, budget, warn_pct);
            let over_multiplier =
                (status == CapStatus::Over)
                    .then(|| r.total_usd / budget.unwrap_or(f64::NAN));
            TicketSpendRow {
                ticket_id: id.clone(),
                title: titles.get(id).cloned(),
                total_usd: r.total_usd,
                runs: r.runs,
                infra_fault_runs: r.infra_fault_runs,
                by_engine: r.by_engine.clone(),
                by_role: r.by_role.clone(),
                last_run_at: r.last_run_at.clone(),
                budget_usd: budget,
                budget_source: source,
                over_multiplier,
                status,
            }
        })
        .collect();

    // Cost desc, id tie-break — mirrors the fleet row ordering contract.
    rows.sort_by(|a, b| {
        b.total_usd
            .partial_cmp(&a.total_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.ticket_id.cmp(&b.ticket_id))
    });

    let total_rows = rows.len();
    let over_count = rows.iter().filter(|r| r.status == CapStatus::Over).count();
    let approaching_count = rows
        .iter()
        .filter(|r| r.status == CapStatus::Approaching)
        .count();
    let total_usd = rows.iter().map(|r| r.total_usd).sum();
    let total_runs: u64 = rows.iter().map(|r| r.runs).sum();

    let limit = limit.clamp(1, MAX_LIMIT);
    rows.truncate(limit);

    TicketSpendPanel {
        rows,
        total_rows,
        total_usd,
        total_runs,
        over_count,
        approaching_count,
        unattributed: TicketSpendUnattributedSummary {
            total_usd: unattributed.total_usd,
            runs: unattributed.runs,
        },
        limit,
    }
}
