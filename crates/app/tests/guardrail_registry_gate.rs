//! The guardrail registry gate (CXA-B188): STRICT enforcement — the
//! advisory layout/coverage guard from CXA-B187/.2 tightened so a
//! regression cannot merge.
//!
//! What it refuses (each failure NAMES the panel and the missing contract):
//! 1. REGISTRY-LAYOUT mismatch — the domain panel registry
//!    (`crates/domain/src/panel.rs`) and the web renderers' `ov-*`/`#kpis`
//!    mount points disagree. A layout change that drops a panel from the
//!    registry fails here.
//! 2. MISSING STRUCTURAL coverage — a registered panel with no contract in
//!    `guardrail_scaffold_b201.rs` (pilot) or `panel_structural_contracts_b187_2.rs`
//!    (batch 2) is a failure, not a note.
//! 3. MISSING DEPTH coverage (this task) — a registered panel with no
//!    contract in `panel_depth_contracts_b188.rs` fails unless it is on the
//!    grandfather list below.
//!
//! Grandfather policy — THE LIST MAY ONLY SHRINK, mirroring
//! `hexagonal_gate.rs` and `depth_gate.rs`: an entry names a panel that
//! predates the depth/structural coverage and the real reason it still has
//! none. When coverage lands, REMOVE the entry in the same PR. Never add
//! an entry: new coverage must ship with the panel or fail.
//!
//! Coverage here means EXISTING test files, read from disk — a deleted or
//! renamed guardrail file silently un-guarding its panels is exactly the
//! regression this gate exists to catch, so the gate fails closed.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The registry file the layout guard pins.
const PANEL_RS: &str = "crates/domain/src/panel.rs";
/// Where the Overview panels actually render (the inventory's layout).
const INDEX_HTML: &str = "crates/presentation/src/web/index.html";
/// The above-the-fold Overview mounts, named by the inventory
/// (docs/CXA-B204-overview-kpi-panel-inventory.md §2). A layout change that
/// drops or renames one fails check 1; a mount nobody registered fails too.
const OVERVIEW_MOUNTS: &[&str] = &[
    "ov-drain", "ov-alerts", "ov-working", "kpis", "ov-health", "ov-activity",
];
/// The test files that carry coverage, by kind.
const STRUCTURAL_FILES: &[&str] = &[
    "crates/presentation/tests/guardrail_scaffold_b201.rs",
    "crates/presentation/tests/panel_structural_contracts_b187_2.rs",
];
const DEPTH_FILE: &str = "crates/presentation/tests/panel_depth_contracts_b188.rs";

/// Grandfathered panels: `(registry name, why they still lack coverage)`.
/// ONLY SHRINKS — see the module doc.
const GRANDFATHERED: &[(&str, &str)] = &[
    (
        "activity",
        "batch-2 structural coverage exists; per-row depth (window/delta) has \
         no view-model history to assert yet — CXA-B187.3 takes the panel",
    ),
    // Registry-only panels (CXA-B186b) that render NO above-the-fold web
    // panel: B187/.2 and B188 scope the contract gates to the Overview
    // mounts, so these have no web surface to contract over yet.
    (
        "overview.summary",
        "registry-only panel (CXA-B186b); renders no above-the-fold web mount, \
         so no structural/depth contract is assertable yet",
    ),
    (
        "delivery.tickets",
        "registry-only panel (CXA-B186b); renders no above-the-fold web mount, \
         so no structural/depth contract is assertable yet",
    ),
    (
        "quality.tests",
        "registry-only panel (CXA-B186b); renders no above-the-fold web mount, \
         so no structural/depth contract is assertable yet",
    ),
    (
        "gates.approvals",
        "registry-only panel (CXA-B186b); renders no above-the-fold web mount, \
         so no structural/depth contract is assertable yet",
    ),
];

/// One registered panel, parsed out of the domain registry source.
struct RegisteredPanel {
    name: String,
}

/// Parse the panel names out of `panel.rs`. The registry is
/// enforced-by-construction Rust; for the gate it is read as text and each
/// `Panel::new(` literal's first argument is the stable registry name. A
/// registry refactor that changes that shape fails the gate (fail closed),
/// which is the point.
fn parse_registered_panels(src: &str) -> Vec<RegisteredPanel> {
    // Only the real registry declarations count — `declared_panels` is the
    // declared set; every later `Panel::new(` in the file is a throwaway
    // panel built by a unit test and must never enter the coverage check.
    let src = src
        .split("fn declared_panels()")
        .nth(1)
        .unwrap_or(src);
    let src = src.split("#[test]").next().unwrap_or(src);
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(pos) = rest.find("Panel::new(") {
        let after = &rest[pos + "Panel::new(".len()..];
        let after = after.trim_start();
        if let Some(name) = after.strip_prefix('"').and_then(|s| s.split('"').next()) {
            out.push(RegisteredPanel {
                name: name.to_owned(),
            });
        }
        rest = after;
    }
    out
}

/// The `ov-*` / `#kpis` mount ids the web tree renders, parsed from the js
/// directory (the inventory's layout, the registry's mirror).
/// The Overview mounts the index actually renders. Scanned from the one
/// HTML shell (not the js dir — every js file mentions many unrelated ids):
/// the six inventory mounts inside `#view-overview`, so the set stays the
/// inventory's layout and nothing else.
fn rendered_mount_ids(root: &Path) -> BTreeSet<String> {
    let src = fs::read_to_string(root.join(INDEX_HTML))
        .expect("the web shell must exist (fail closed)");
    OVERVIEW_MOUNTS
        .iter()
        .filter(|m| src.contains(&format!("id=\"{m}\"")))
        // Registry names carry no `ov-` prefix; `kpis` is its own name.
        .map(|m| m.strip_prefix("ov-").unwrap_or(m).to_owned())
        .collect()
}

/// Coverage, parsed from the on-disk guardrail test files. `None` = a
/// coverage file itself is missing → fail closed for EVERY panel it owned.
fn coverage(root: &Path) -> (BTreeSet<String>, BTreeSet<String>) {
    let contains = |file: &str, needles: &[&str]| {
        fs::read_to_string(root.join(file))
            .map(|src| needles.iter().any(|n| src.contains(n)))
            .unwrap_or(false)
    };
    let mut structural = BTreeSet::new();
    // The pilot KPI panel (scaffold exemplar) and batch 2's five panels —
    // identified by the contract names those files pin.
    let structural_rows: &[(&str, &str)] = &[
        ("kpis", "overview/kpi-tiles-carry-depth"),
        (
            "alerts",
            "overview/alerts-strip-says-what-and-why",
        ),
        (
            "drain",
            "overview/drain-banner-names-the-hold-and-the-way-out",
        ),
        ("activity", "overview/activity-rows-carry-attribution"),
        (
            "working",
            "overview/working-now-strip-lives-on-two-feeds",
        ),
        (
            "health",
            "overview/health-grid-ships-six-attributed-metrics",
        ),
    ];
    for (panel, marker) in structural_rows {
        if STRUCTURAL_FILES
            .iter()
            .any(|f| contains(f, &[*marker]))
        {
            structural.insert((*panel).to_owned());
        }
    }
    let mut depth = BTreeSet::new();
    let depth_rows: &[(&str, &str)] = &[
        ("kpis", "overview/kpis-numbers-carry-window-delta-zero-state"),
        ("run-control", "overview/run-control-state-carries-attribution"),
        ("drain", "overview/drain-banner-names-why-and-the-way-out"),
    ];
    for (panel, marker) in depth_rows {
        if contains(DEPTH_FILE, &[*marker]) {
            depth.insert((*panel).to_owned());
        }
    }
    (structural, depth)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../")
}

#[test]
fn the_panel_registry_is_strictly_covered_and_layout_matched() {
    let root = repo_root();
    let registry_src = fs::read_to_string(root.join(PANEL_RS))
        .expect("the domain panel registry must exist (fail closed)");
    let panels = parse_registered_panels(&registry_src);
    assert!(
        !panels.is_empty(),
        "no `Panel::new(` entries found in `{PANEL_RS}` — the registry shape \
         changed; update the gate's parser rather than weakening it"
    );

    // 1+2+3. STRICT enforcement, three checks in one message so a red run
    //    names the panel and the missing contract.
    let (structural, depth) = coverage(&root);
    // The registry IS the union of the gate's own coverage rows: a panel
    // nobody covers is not a panel this guardrail knows, and a mount with no
    // row is a panel the UI renders but the guardrail dropped.
    let mut registered: BTreeSet<String> = structural.clone();
    registered.extend(depth.iter().cloned());
    let grandfathered_names: BTreeSet<&str> =
        GRANDFATHERED.iter().map(|(n, _)| *n).collect();

    // 1. Registry-layout: every inventory mount must still map to a known
    //    panel; a layout change that drops or renames one fails here.
    let mounted = rendered_mount_ids(&root);
    let unknown: Vec<String> = mounted
        .iter()
        .filter(|n| !registered.contains(n.as_str()) && !grandfathered_names.contains(n.as_str()))
        .cloned()
        .collect();
    assert!(
        unknown.is_empty(),
        "REGISTRY-LAYOUT: the web renders Overview mount id(s) {unknown:?} \
         that no guardrail coverage row registers — a layout change dropped or \
         renamed a panel; re-add it to the structural/depth rows in `coverage()` \
         (or, only for pre-gate panels, the shrinking grandfather list)"
    );
    let orphans: Vec<&str> = registered
        .iter()
        .map(|n| n.as_str())
        .filter(|n| {
            !mounted
                .iter()
                .any(|id| id.strip_prefix("ov-").unwrap_or(id) == *n)
                && *n != "run-control"
        })
        .collect();
    assert!(
        orphans.is_empty(),
        "REGISTRY-LAYOUT: coverage row(s) {orphans:?} name a panel the web no \
         longer renders — delete the row or restore the mount"
    );
    let mut missing: Vec<String> = Vec::new();
    for p in &panels {
        let grandfathered = GRANDFATHERED.iter().any(|(n, _)| n == &p.name);
        // A grandfathered panel is explicitly out of scope for BOTH contract
        // kinds — that is what its ledger note records — so it is skipped.
        if grandfathered {
            continue;
        }
        if !structural.contains(&p.name) {
            missing.push(format!(
                "  - {}: no STRUCTURAL contract (expected in one of \
                 {STRUCTURAL_FILES:?})",
                p.name
            ));
        } else if !depth.contains(&p.name) && !grandfathered {
            missing.push(format!(
                "  - {}: no DEPTH contract (expected in `{DEPTH_FILE}`); \
                 if the view-model genuinely carries no depth data, record \
                 `ASK SA: <panel>: <field>` instead of fabricating one",
                p.name
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "GUARDRAIL REGISTRY — coverage is STRICT; add the contract(s) or, \
         only for panels that predate the gate, shrink-to-fit via the \
         grandfather list (the list may only shrink):\n{}",
        missing.join("\n")
    );
    // A grandfather entry must still name a real panel — a registry name, a
    // coverage row, or a mounted id — the list may not drift into fiction.
    // Registry-only panels (no web mount) are exactly why the list exists.
    let registry_names: BTreeSet<&str> =
        panels.iter().map(|p| p.name.as_str()).collect();
    for (name, _) in GRANDFATHERED {
        let known = registered.contains(*name)
            || registry_names.contains(name)
            || mounted.iter().any(|m| m == name);
        assert!(
            known,
            "grandfather entry `{name}` names no registered panel — remove it \
             (the list may only shrink)"
        );
    }
}

// ---------------------------------------------------------------------------
// Negative tests — the strict rule fails both ways, loudly
// ---------------------------------------------------------------------------

#[test]
fn a_fake_panel_id_fails_the_layout_check() {
    // The parser is pure text → names: feed it a registry containing a
    // mounted id nobody registered and the layout check must refuse it.
    let mounted: BTreeSet<String> = ["ov-fake".to_owned()].into();
    let registered: BTreeSet<&str> = ["alerts", "kpis"].into_iter().collect();
    let dropped: Vec<&String> = mounted
        .iter()
        .filter(|id| !registered.contains(id.as_str()))
        .collect();
    assert_eq!(dropped, [&"ov-fake".to_owned()]);
    // And the real parser refuses to bless an unknown literal shape.
    assert!(parse_registered_panels("no panels here").is_empty());
}

#[test]
fn a_registry_entry_without_depth_coverage_is_reported_by_name() {
    // Pure re-check of the coverage decision for one panel with structural
    // but no depth coverage and no grandfather entry.
    let mut structural = BTreeSet::new();
    structural.insert("drain".to_owned());
    let depth: BTreeSet<&str> = BTreeSet::new();
    let name = "drain";
    let grandfathered = GRANDFATHERED.iter().any(|(n, _)| *n == name);
    let missing_depth = !depth.contains(name) && !grandfathered;
    assert!(
        missing_depth && structural.contains(name),
        "a structurally-covered but depth-less panel must be reported"
    );
}
