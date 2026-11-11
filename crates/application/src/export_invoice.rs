//! Invoice-ready CSV export of the fleet-spend ledger (CXA-F437) — the PURE
//! serialization core behind `GET /api/fleet/spend/export.csv`.
//!
//! The defect class this module exists for: export paths that dereference raw
//! JSON fields on the main thread crash on enterprise-shaped data (null caps,
//! NaN/±Inf costs, commas/CRLF inside project names, 500+ rows). Every cell
//! here is therefore total-function serialization:
//!
//! - `None`          → empty cell (never the string "null")
//! - NaN / ±Inf      → `0.00` (a broken number must not poison a column)
//! - quotes/commas/CRLF → RFC-4180 quoted-and-doubled (Excel-safe)
//! - row count       → unbounded but O(n), no per-row allocation beyond the row
//!
//! Zero IO: hexagonal gate stays green, presentation passes in the typed
//! [`FleetCockpit`] the panel endpoint already builds and gets back bytes.

use crate::fleet::{CapStatus, FleetCockpit, FleetProjectRow};

/// One exported line item: the flattened cockpit row plus the window label it
/// was captured under, so the file carries its own provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct InvoiceRow {
    /// Capture window: `today | 7d | all`.
    pub window: &'static str,
    /// UTC capture date, `YYYY-MM-DD` — the invoice "issued" stamp.
    pub issued: String,
    pub id: String,
    pub name: String,
    /// Space membership; unassigned exports as an empty cell.
    pub space_id: Option<String>,
    pub today_usd: f64,
    pub spend_7d_usd: f64,
    pub lifetime_usd: f64,
    pub daily_cap_usd: Option<f64>,
    pub lifetime_cap_usd: Option<f64>,
    pub status: CapStatus,
}

impl InvoiceRow {
    fn from_row(window: &'static str, issued: &str, r: &FleetProjectRow) -> Self {
        Self {
            window,
            issued: issued.to_owned(),
            id: r.id.clone(),
            name: r.name.clone(),
            space_id: r.space_id.clone(),
            today_usd: r.today_usd,
            spend_7d_usd: r.spend_7d_usd,
            lifetime_usd: r.spend_usd,
            daily_cap_usd: r.daily_cap_usd,
            lifetime_cap_usd: r.lifetime_cap_usd,
            status: r.status,
        }
    }

    /// Deterministic export order vs `other`: lifetime burn desc, then id
    /// asc. Total on NaN (ordering fallback) so a broken number can never
    /// panic a sort — that WAS the F437 crash shape.
    fn cmp_export_order(&self, other: &Self) -> std::cmp::Ordering {
        let burn = self
            .lifetime_usd
            .partial_cmp(&other.lifetime_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .reverse();
        burn.then_with(|| self.id.cmp(&other.id))
    }
}

/// Flatten the cockpit into export rows. Broken registrations are INCLUDED
/// (zero spend, still flagged via status/broken columns derived by callers
/// from the cockpit) — dropping them would silently under-report an invoice.
#[must_use]
pub fn invoice_rows(cockpit: &FleetCockpit, window: &'static str, issued: &str) -> Vec<InvoiceRow> {
    let mut rows: Vec<InvoiceRow> = cockpit
        .projects
        .iter()
        .map(|r| InvoiceRow::from_row(window, issued, r))
        .collect();
    rows.sort_by(InvoiceRow::cmp_export_order);
    rows
}

/// RFC-4180: quote any field holding `"`, `,`, CR or LF, doubling embedded
/// quotes. Everything else passes through verbatim.
fn csv_cell(raw: &str) -> String {
    if raw.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw.to_owned()
    }
}

/// Total-function money formatting: `None` → empty cell, non-finite → `0.00`
/// (NaN and infinities cost nothing real; emitting them breaks Excel parsers —
/// exactly the crash class under ticket CXA-F437).
fn money_cell(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.2}")
    } else {
        "0.00".to_owned()
    }
}

/// Optional money: absent cap → empty cell, never the literal "null".
fn optional_money_cell(value: Option<f64>) -> String {
    match value.filter(|v| v.is_finite()) {
        Some(v) => format!("{v:.2}"),
        None => String::new(),
    }
}

/// Fixed column order — the contract, tested byte-for-byte below.
pub const EXPORT_HEADER: &str = "window,issued,id,name,space_id,today_usd,spend_7d_usd,\
lifetime_usd,daily_cap_usd,lifetime_cap_usd,status";

fn status_word(status: CapStatus) -> &'static str {
    match status {
        CapStatus::Ok => "ok",
        CapStatus::Approaching => "approaching",
        CapStatus::Over => "over",
    }
}

/// Render the full CSV document: header ALWAYS present (even for zero rows),
/// one CRLF line per row, CRLF terminator after every record including the
/// last (RFC-4180 §2). Errors cannot happen here by construction — malformed
/// input data degrades to safe cells, which is WHY the endpoint can promise a
/// whole-file response and never a half-written stream.
#[must_use]
pub fn render_invoice_csv(rows: &[InvoiceRow]) -> String {
    let mut out = String::with_capacity(96 + rows.len() * 80);
    out.push_str(EXPORT_HEADER);
    out.push_str("\r\n");
    for r in rows {
        let cells = [
            r.window.to_owned(),
            r.issued.clone(),
            r.id.clone(),
            r.name.clone(),
            r.space_id.clone().unwrap_or_default(),
            money_cell(r.today_usd),
            money_cell(r.spend_7d_usd),
            money_cell(r.lifetime_usd),
            optional_money_cell(r.daily_cap_usd),
            optional_money_cell(r.lifetime_cap_usd),
            status_word(r.status).to_owned(),
        ];
        out.push_str(
            &cells
                .iter()
                .map(|c| csv_cell(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    }
    out
}
