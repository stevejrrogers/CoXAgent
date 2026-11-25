//! CXA-B181.2 — presentation guardrails for the above-the-fold KPI tiles
//! (`#kpis`, `crates/presentation/src/web/js/kpis.js`).
//!
//! Panel-list source: CXA-B181.1's enumerated fold set
//! (`crates/application/tests/overview_fold_guardrails_b181.rs`, panel 1).
//! Convention: the same no-harness `include_str!` discipline as
//! `worklog_panel_terminal_state_b131.rs` — pin the bytes the hub serves and
//! the view-model expressions those bytes are built from. No server, no port,
//! no invented types.
//!
//! Depth-bar contract asserted here: visible counts with a signed delta and
//! its window label, zero-state hints that say what would fill the number,
//! and click-through drill-down on every tile.

// Test scratch-parsing of shipped JS/HTML bytes: an `expect` here fails the
// test loudly, which is exactly what a guardrail should do (repo convention).
#![allow(clippy::expect_used, clippy::unwrap_used)]

/// The dashboard shell the router serves verbatim.
const INDEX_HTML: &str = include_str!("../src/web/index.html");

/// The KPI tile renderer — owner of the fold's headline numbers.
const KPIS_JS: &str = include_str!("../src/web/js/kpis.js");

/// The overview render path — owner of the fold's zero states.
const CORE_JS: &str = include_str!("../src/web/js/core.js");

/// AC(depth 2+3): every tile carries a signed delta against a NAMED window —
/// the number never ships bare. `overviewKpiTile` must render the delta span
/// next to the label plus the "vs prior 14d" window label, and the delta
/// formatter must be sign-explicit ("±0" for zero, not a silent blank).
#[test]
fn every_kpi_carries_a_signed_delta_against_a_named_window() {
    assert!(
        KPIS_JS.contains("vs prior 14d"),
        "each tile must label its comparison window — a bare number has no \
         depth (CXA-B181.2 depth bar: a NUMBER ships with its window)"
    );
    assert!(
        KPIS_JS.contains("function signedNum(n){return n>0?\"+\"+n:n<0?\"-\"+(-n):\"±0\";}",),
        "the delta formatter must be sign-explicit including the ±0 case — \
         a zero delta that renders blank reads as a broken tile"
    );
    assert!(
        KPIS_JS.contains(
            "function signedMoney(n){return n>0?\"+\"+money(n):n<0?\"-\"+money(-n):\"±0\";}"
        ),
        "the money tile's delta needs the same sign discipline"
    );
    // The tile template must actually EMIT both the headline value and the
    // delta span with its colour class — the window label alone is not enough.
    assert!(
        KPIS_JS.contains("<div class=\"v\">") && KPIS_JS.contains("class=\"${cls}\""),
        "the tile template must render the value and the signed delta class \
         (kd up / kd dn / kd z) together"
    );
}

/// AC(depth 5): the fold's zero state is explanatory, not blank — when the
/// project has no tickets and no activity the tiles area says what the team
/// is and what fills it ("Press Start to run the loop…").
#[test]
fn kpi_zero_state_says_what_would_fill_the_numbers() {
    assert!(
        CORE_JS.contains("Ready to build"),
        "the fold's zero state must carry the 'Ready to build' headline"
    );
    assert!(
        CORE_JS.contains("Press <b style=\"color:var(--text)\">Start</b> to run the loop"),
        "the zero state must name the action that fills the panel (Start the \
         loop), not leave a blank grid"
    );
    assert!(
        CORE_JS.contains("Set engines and workflow in"),
        "the zero state must point at the setup path (Settings) so an empty \
         project is a next step, not a dead end"
    );
}

/// AC(depth 5, per-tile): each tile ships a hint naming the event that fills
/// its number — the zero-state hint the depth bar demands instead of a bare 0.
#[test]
fn every_tile_hint_names_what_fills_it() {
    for hint in [
        "ships land here when a ticket reaches documented",
        "work lands here when a ticket is readied for an agent",
        "docs land here when DOCS documents a shipped ticket",
        "releases land here when a version is tagged",
        "cost accrues here as agent runs burn tokens",
    ] {
        assert!(
            KPIS_JS.contains(hint),
            "missing tile hint: {hint:?} — every KPI must say what would fill it"
        );
    }
}

/// AC(depth 4): the tiles are a bounded window onto larger lists — each tile
/// must carry a click-through drill-down (nav to the owning view), and the
/// Releases tile must scroll to the changelog panel. A count without a way
/// to see its rows is a dead end.
#[test]
fn every_tile_drills_down_to_its_owning_view() {
    let navs = KPIS_JS.matches("nav('").count();
    assert!(
        navs >= 4,
        "at least four tiles must drill down (board/docs/changelog/insights), got {navs}"
    );
    assert!(
        KPIS_JS.contains("ov-changelog').scrollIntoView"),
        "the Releases tile must scroll to the changelog panel — its list lives \
         below the fold"
    );
    assert!(
        KPIS_JS.contains("nav('insights')"),
        "the Cost tile must drill into the Insights view where the spend \
         series is charted"
    );
}

/// AC(depth 1, fold seam): the tiles container is the fold's first-class slot
/// in the shipped shell, above the diagnostics fold — the panel list from
/// CXA-B181.1 stays anchored in the real markup.
#[test]
fn the_tiles_slot_is_anchored_above_the_diagnostics_fold() {
    let kpis = INDEX_HTML
        .find("id=\"kpis\"")
        .expect("#kpis slot must exist");
    let fold_end = INDEX_HTML
        .find("Diagnostics &amp; gates")
        .expect("the diagnostics fold must exist");
    assert!(
        kpis < fold_end,
        "the KPI tiles must render above the Diagnostics fold — CXA-F383 put \
         everything diagnostic below the fold and B181.1's panel list pins it"
    );
}
