//! CXA-B293 — regression test for CXA-B278: the promised render-order unit
//! test for `OverviewPanelId` (CXA-B228) exists again.
//!
//! The module doc of `crates/domain/src/overview_panel.rs` cites this file as
//! pinning the declared render order to the enum's case order; after B278's
//! fix landed, a later history dropped the file and the citation dangled.
//! This restores it, with regression coverage going past the module's inline
//! tests:
//!
//! 1. `RENDER_ORDER` equals the enum's *declared* case order — literally, not
//!    just set-equal — so reordering or inserting a variant forces a conscious
//!    edit of both together (the enumeration is the contract).
//! 2. `RENDER_ORDER` covers every variant exhaustively, via the compiler's
//!    match-exhaustiveness itself (a new silent variant fails here).
//! 3. Each panel maps to its unique DOM id in lock-step with the index.html
//!    mount order; no two panels share a mount.
//!
//! Pure functions over the real domain type (`OverviewPanelId`) — no IO, no
//! framework, no fixtures beyond the type itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_domain::overview_panel::OverviewPanelId;

/// AC1 (B228): `RENDER_ORDER` is exactly the enum's case order, position by
/// position — `assert_eq!` on slices compares element-wise, so an internal
/// swap inside `RENDER_ORDER` cannot pass while matching as a set.
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

/// AC2: `RENDER_ORDER` is exhaustive over the variants. Match arms enumerate
/// the variants literally, so adding an eighth variant makes this fail to
/// compile until it is placed in the render order consciously (match has no
/// wildcard arm by design). If the fixture below drifts from the actual case
/// order, the AC1 assertion catches that too.
#[test]
fn render_order_is_exhaustive_over_every_variant() {
    let mut declared = OverviewPanelId::RENDER_ORDER.iter().copied();
    let exhaustiveness_witness = [
        OverviewPanelId::OvDrain,
        OverviewPanelId::OvAlerts,
        OverviewPanelId::OvWorking,
        OverviewPanelId::Kpis,
        OverviewPanelId::OvHealth,
        OverviewPanelId::OvActivity,
        OverviewPanelId::OvChangelog,
    ];

    for panel in exhaustiveness_witness {
        // Naming each variant compiles only while the variant exists;
        // dropping one without updating RENDER_ORDER breaks the length and
        // membership assertions below.
        #[allow(clippy::match_single_binding)]
        match panel {
            OverviewPanelId::OvDrain
            | OverviewPanelId::OvAlerts
            | OverviewPanelId::OvWorking
            | OverviewPanelId::Kpis
            | OverviewPanelId::OvHealth
            | OverviewPanelId::OvActivity
            | OverviewPanelId::OvChangelog => {}
        }
        assert!(
            OverviewPanelId::RENDER_ORDER.contains(&panel),
            "{panel:?} is missing from RENDER_ORDER"
        );
        assert_eq!(
            Some(panel),
            declared.next(),
            "fixture order drifted from RENDER_ORDER"
        );
    }
    assert!(
        declared.next().is_none(),
        "RENDER_ORDER holds entries that are not enum variants' positions"
    );
}

/// AC3: `dom_id` agrees with the ground-truth mounts in
/// `index.html#view-overview`, in the same order as the declaration, and the
/// ids stay unique across all seven panels.
#[test]
fn dom_ids_match_the_index_html_mount_order_and_stay_unique() {
    const MOUNTS_IN_DOM_ORDER: [&str; 7] = [
        "ov-drain",
        "ov-alerts",
        "ov-working",
        "kpis",
        "ov-health",
        "ov-activity",
        "ov-changelog",
    ];

    let rendered: Vec<_> = OverviewPanelId::RENDER_ORDER
        .iter()
        .map(|p| p.dom_id())
        .collect();

    assert_eq!(
        rendered.len(),
        MOUNTS_IN_DOM_ORDER.len(),
        "render order lost or gained a mount"
    );
    for (i, (got, want)) in rendered.iter().zip(MOUNTS_IN_DOM_ORDER).enumerate() {
        assert_eq!(*got, want, "position {i}: DOM mount order changed");
    }

    let mut sorted = rendered.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        rendered.len(),
        "duplicate dom_id in RENDER_ORDER"
    );
}
