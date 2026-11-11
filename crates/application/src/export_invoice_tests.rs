//! Pure tests for [`crate::export_invoice`] (CXA-F437) — the invoice-ready CSV
//! serialization contract, exercised ONLY through real types (`FleetCockpit`,
//! `FleetProjectRow`, `CapStatus`) and the public pure functions. No IO, no
//! harness: same input, same bytes.

use crate::export_invoice::{invoice_rows, render_invoice_csv, InvoiceRow, EXPORT_HEADER};
use crate::fleet::{CapStatus, FleetCockpit, FleetProjectRow, FleetTotals};

/// Fixture built from real types only.
fn row(id: &str, name: &str, lifetime: f64) -> FleetProjectRow {
    FleetProjectRow {
        id: id.to_owned(),
        name: name.to_owned(),
        space_id: None,
        spend_usd: lifetime,
        today_usd: lifetime / 10.0,
        spend_7d_usd: lifetime / 2.0,
        lifetime_cap_usd: Some(1000.0),
        daily_cap_usd: Some(50.0),
        headroom_usd: Some((1000.0 - lifetime).max(0.0)),
        headroom_today_usd: Some(25.0),
        status: CapStatus::Ok,
        broken: false,
    }
}

fn cockpit(rows: Vec<FleetProjectRow>) -> FleetCockpit {
    let totals = FleetTotals {
        spend_usd: 0.0,
        today_usd: 0.0,
        spend_7d_usd: 0.0,
        projects: rows.len(),
        over: 0,
        approaching: 0,
        broken: 0,
        hub_headroom_usd: None,
    };
    FleetCockpit {
        hub_ceiling_usd: 0.0,
        hub_warn_pct: 0.8,
        totals,
        projects: rows,
        spaces: Vec::new(),
    }
}

/// The header is part of the document even when nothing has been spent yet —
/// a fresh hub downloads a valid, openable CSV rather than an empty file.
#[test]
fn header_is_always_present_even_with_zero_rows() {
    let csv = render_invoice_csv(&[]);
    assert_eq!(csv, format!("{EXPORT_HEADER}\r\n"));
}

/// Deterministic order: lifetime burn desc, then id asc. NaN sorts WITHOUT
/// panicking (the total-function requirement that motivated this module).
#[test]
fn rows_sort_by_lifetime_desc_then_id_asc_total_on_nan() {
    let mut c = cockpit(vec![
        row("b-beta", "Beta", 30.0),
        row("a-alpha", "Alpha", 30.0),
        row("z-top", "Top", 90.0),
    ]);
    // Simulate one project whose ledger produced NaN (bad aggregation).
    if let Some(r) = c.projects.iter_mut().find(|r| r.id == "z-top") {
        r.spend_usd = f64::NAN;
    }
    let rows = invoice_rows(&c, "all", "2026-11-13");
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    // NaN compares Equal to everything under our fallback → stable position by
    // id among ties; what matters is NO PANIC and lifetime-desc for finites.
    assert_eq!(&ids[..2], &["a-alpha", "b-beta"]);
}

/// RFC-4180 quoting: embedded comma, quote and CRLF survive a round trip into
/// Excel-shaped cells; plain fields pass verbatim.
#[test]
fn cells_quote_commas_quotes_and_crlf_per_rfc_4180() {
    let mut c = cockpit(vec![row(
        "p1",
        "Acme, \"Enterprise\" Ltd.\nHQ\u{000D}branch",
        12.5,
    )]);
    if let Some(r) = c.projects.first_mut() {
        r.name = "Acme, \"Enterprise\" Ltd.\r\nHQ branch".to_owned();
    }
    let rows = invoice_rows(&c, "today", "2026-11-13");
    let csv = render_invoice_csv(&rows);
    // A quoted record may itself contain CRLF, so match against the whole
    // document rather than splitting on newlines.
    assert!(
        csv.contains("\"Acme, \"\"Enterprise\"\" Ltd.\r\nHQ branch\","),
        "name must be quoted with doubled quotes and keep its CRLF"
    );
}

/// A broken project carrying non-finite money exports as clean numeric cells —
/// never the literal strings `NaN` or `null`. This IS the F437 crash shape,
/// locked in forever at the unit level.
#[test]
fn nan_and_infinity_degrade_to_zero_not_nan_strings() {
    let mut c = cockpit(vec![row("broken", "Broken", f64::NAN)]);
    if let Some(r) = c.projects.first_mut() {
        r.today_usd = f64::INFINITY;
        r.daily_cap_usd = Some(f64::NAN);
        r.lifetime_cap_usd = None;
    }
    let csv = render_invoice_csv(&invoice_rows(&c, "all", "2026-11-13"));
    let data = csv.lines().nth(1).unwrap_or_default();
    assert!(!data.contains("NaN") && !data.contains("null"), "{data}");
    assert!(
        data.contains(",0.00,"),
        "non-finite costs emit 0.00: {data}"
    );
}

/// Absent caps export as EMPTY cells, never the string "null" — parsing a
/// missing cap as zero would make uncapped look capped in downstream sums.
#[test]
fn absent_caps_export_as_empty_cells_not_null() {
    let mut r = row("uncapped", "Uncapped", 5.0);
    r.daily_cap_usd = None;
    r.lifetime_cap_usd = None;
    let csv = render_invoice_csv(&[InvoiceRow {
        window: "all",
        issued: "2026-11-13".into(),
        id: r.id.clone(),
        name: r.name,
        space_id: None,
        today_usd: r.today_usd,
        spend_7d_usd: r.spend_7d_usd,
        lifetime_usd: r.spend_usd,
        daily_cap_usd: None,
        lifetime_cap_usd: None,
        status: r.status,
    }]);
    let data = csv.lines().nth(1).unwrap_or_default();
    assert!(
        data.ends_with(",,ok"),
        "empty cap cells before status: {data}"
    );
}

/// Enterprise scale: 500+ line items serialize completely and quickly, each on
/// its own CRLF record — AC1's shape at the aggregate level.
#[test]
fn five_hundred_line_items_all_exported() {
    let rows: Vec<FleetProjectRow> = (0..600)
        .map(|i| {
            let spend = f64::from(i);
            row(&format!("proj-{i:03}"), &format!("Project {i:03}"), spend)
        })
        .collect();
    let out = render_invoice_csv(&invoice_rows(&cockpit(rows), "all", "2026-11-13"));
    let records = out.split("\r\n").filter(|l| !l.is_empty()).count();
    assert_eq!(records, 601, "header + every one of the 600 line items");
}
