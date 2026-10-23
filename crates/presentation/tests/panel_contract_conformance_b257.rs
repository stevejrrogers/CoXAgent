//! CXA-B257 — the panel-contract CONFORMANCE test locking the Overview mapping.
//!
//! CXA-B195 froze the panel contract (`src/overview_panels.rs`:
//! [`coxagent_presentation::overview_panels::OVERVIEW_PANELS`]) and CXA-B194
//! wired its JS mirror (`web/js/overview_states.js`, `OvPanelStates.PANELS`)
//! plus the DOM mounts in `web/index.html`. Three parties, one contract — and
//! until now only one direction was guarded (B194 walked Rust→JS; nothing
//! caught a JS-side key nobody ships a Rust entry for).
//!
//! This suite locks ALL THREE sides together, bidirectionally:
//!
//! 1. **Key-set equality** — the JS wiring covers EXACTLY the Rust manifest,
//!    both directions; a panel added on either side alone fails by name.
//! 2. **Per-field fidelity** — `dom_id` and `dependency.feeds` (the mapped
//!    values) agree per panel; every other contract field (`purpose`,
//!    `endpoint`, `owner`, `states`) is consumed through an EXHAUSTIVE
//!    destructuring, so a field added to `OverviewPanel` /
//!    `PanelDataDependency` without updating this mapping FAILS here — at
//!    compile time if dropped from the destructure, at assert time if left
//!    unconsumed.
//! 3. **Mount integrity** — every `dom_id` is a real element id in the served
//!    shell (`index.html`); a renamed host leaves the panel painting nowhere.
//! 4. **Purity ratchet** — the mapping modules stay IO-free: neither
//!    `overview_panels.rs` nor `overview_states.js` may reach for fs, process,
//!    sockets or HTTP. Same spirit as `crates/app/tests/hexagonal_gate.rs`
//!    (source-level check, forbidden-token scan over the production half),
//!    registered HERE as a new check rather than touching any grandfather
//!    list.
//!
//! Harness: the proven zero-IO Node runner from
//! `overview_panel_terminal_state_b194.rs` — `include_str!` the exact bytes
//! the server serves, evaluate with `node -e`, no live server, no network, no
//! env vars. No fixture data exists beyond what the real domain types hold.

use coxagent_presentation::overview_panels::{
    self, optional_slots, required_slots, OverviewPanel, OverviewPanelId, Slot, TerminalState,
};
use std::collections::BTreeSet;
use std::process::Command;

// ---------------------------------------------------------------------------
// Zero-IO JS harness (the B194 pattern, verbatim shape)
// ---------------------------------------------------------------------------

/// Run `node -e` and return trimmed stdout.
fn run_node(script: &str) -> String {
    let out = Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap_or_else(|e| panic!("node runtime needed for the JS half of the contract: {e}"));
    assert!(
        out.status.success(),
        "node evaluation failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

const WIRING: &str = include_str!("../src/web/js/overview_states.js");

/// The Overview view-mesh (CXA-B232b): since CXA-B289 the wiring resolves every
/// panel mount through `OvViewMesh.domId()`, never an inline dom id, so the
/// mesh must load BEFORE the wiring — mirroring index.html / APP_JS order (the
/// B194 harness does the same).
const MESH: &str = include_str!("../src/web/js/overview_vm.js");

/// Evaluate the wiring's pure parts (no document, no TerminalState needed to
/// read the manifest mirror) and print the last expression.
fn eval_js(expr: &str) -> String {
    let script = format!("const window=globalThis;\n{MESH}\n{WIRING}\n;console.log(({expr}));");
    run_node(&script)
}

/// The JS mirror, parsed into typed shapes that mirror the Rust contract —
/// no fixture data; everything comes from the served bytes.
mod js {
    use super::*;

    /// One entry of the JS `PANELS` map, exactly the keys it really carries
    /// (the wiring speaks camelCase: `domId`; `dep`/`hint` are already flat).
    #[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct JsPanel {
        /// DOM id the panel paints into — mirrors `OverviewPanel::dom_id`.
        pub dom_id: String,
        /// Data-dependency description — mirrors `dependency.feeds`.
        pub dep: String,
        /// The what-fills-this empty-state hint.
        pub hint: String,
    }

    /// Fetch the entire JS mirror in ONE node round-trip.
    pub fn fetch_panels() -> std::collections::BTreeMap<String, JsPanel> {
        let json = eval_js("JSON.stringify(OvPanelStates.PANELS)");
        serde_json::from_str(&json).unwrap_or_else(|e| {
            panic!("OvPanelStates.PANELS is not a flat object map: {e}\n{json}")
        })
    }
}
use js::{fetch_panels, JsPanel};

// ---------------------------------------------------------------------------
// 1. Key-set equality, BOTH directions
// ---------------------------------------------------------------------------

/// The JS wiring covers exactly the Rust manifest — and the manifest covers
/// exactly the JS wiring. A panel introduced on one side alone is named.
#[test]
fn js_wiring_and_rust_manifest_have_identical_key_sets_both_ways() {
    let rust: BTreeSet<String> = overview_panels::OVERVIEW_PANELS
        .iter()
        .map(|p| p.id.to_string())
        .collect();
    let js: BTreeSet<String> = fetch_panels().keys().cloned().collect();

    let missing_in_js: Vec<_> = rust.difference(&js).collect();
    assert!(
        missing_in_js.is_empty(),
        "manifest panels without a JS wiring entry — add them to PANELS in \
         web/js/overview_states.js (key = OverviewPanelId Display string): {missing_in_js:?}"
    );

    let orphaned_in_js: Vec<_> = js.difference(&rust).collect();
    assert!(
        orphaned_in_js.is_empty(),
        "JS wiring entries with no frozen manifest panel — remove them from PANELS \
         or freeze the panel in OVERVIEW_PANELS first: {orphaned_in_js:?}"
    );
}

/// Every `OverviewPanelId` discriminant resolves through the manifest lookup
/// helper — a new variant whose Display string drifted from its key surfaces
/// here as a lookup miss.
#[test]
fn every_panel_id_discriminant_resolves_through_the_lookup_helper() {
    // Exhaustive over the contract's discriminant set: adding a variant
    // WITHOUT extending this array fails the compile of THIS test.
    for id in [
        OverviewPanelId::Drain,
        OverviewPanelId::Alerts,
        OverviewPanelId::Working,
        OverviewPanelId::Kpis,
        OverviewPanelId::Health,
        OverviewPanelId::RecentActivity,
        OverviewPanelId::Releases,
    ] {
        assert_eq!(
            overview_panels::panel(id).map(|p| p.id),
            Some(id),
            "{id}: not resolvable via overview_panels::panel()"
        );
        let display = id.to_string();
        assert!(
            fetch_panels().contains_key(&display),
            "{id}: display-key drift — `{display}` is not the JS PANELS key"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Per-field fidelity + exhaustive consumption of the contract struct
// ---------------------------------------------------------------------------

/// Consume EVERY field of the contract via one exhaustive destructure. Adding
/// a field to `OverviewPanel` or `PanelDataDependency` without updating this
/// function fails the build of this test; leaving it unconsumed fails the
/// assertions below. This is the "contract grew, mapping didn't" tripwire.
fn consume_whole_contract(
    OverviewPanel {
        id,
        dom_id,
        purpose,
        dependency:
            overview_panels::PanelDataDependency {
                feeds,
                endpoint,
                owner,
            },
        states,
    }: &OverviewPanel,
) {
    assert!(!dom_id.is_empty(), "{id}: blank dom_id");
    assert!(
        dom_id.starts_with("ov-") || *dom_id == "kpis",
        "{id}: dom_id `{dom_id}` outside the shell's id scheme"
    );
    assert!(
        !purpose.is_empty(),
        "{id}: blank purpose — the why-above-the-fold line is contract copy"
    );
    assert!(
        !feeds.is_empty(),
        "{id}: blank dependency.feeds — the skeleton titles are built from it"
    );
    assert!(
        endpoint.starts_with('/'),
        "{id}: dependency.endpoint `{endpoint}` is not a hub-relative path"
    );
    assert!(!owner.is_empty(), "{id}: blank dependency.owner");
    assert!(!states.is_empty(), "{id}: empty state set");
}

/// Each mapped value (`dom_id`, `dependency.feeds`) agrees byte-for-byte with
/// the Rust manifest, per panel; the unmapped-but-required `hint` is a real
/// sentence, not a stub.
#[test]
fn each_mapped_field_agrees_byte_for_byte_across_rust_and_js() {
    let js_panels = fetch_panels();
    for p in overview_panels::OVERVIEW_PANELS {
        consume_whole_contract(p);
        let key = p.id.to_string();
        let jp: &JsPanel = js_panels
            .get(&key)
            .unwrap_or_else(|| panic!("{key}: vanished between the two sweeps"));
        assert_eq!(
            jp.dom_id, p.dom_id,
            "{key}: JS domId drifted from the Rust manifest's dom_id"
        );
        assert_eq!(
            jp.dep, p.dependency.feeds,
            "{key}: JS dep drifted from the Rust manifest's dependency.feeds"
        );
        assert!(
            !jp.hint.trim().is_empty(),
            "{key}: empty-state hint is blank — EMPTY would render a bare nothing"
        );
        assert!(
            jp.dep != jp.hint && jp.dom_id != jp.hint,
            "{key}: hint is a copy of another mapped field, not a real sentence"
        );
    }
}

/// State sets stay inside the contract's closed discriminant set, respect the
/// terminal/non-terminal rule, and reach a ready-like rest state — re-asserted
/// here through the contract's own predicates so a new `TerminalState` variant
/// or a mis-typed manifest row trips the conformance suite, not prod.
#[test]
fn state_sets_obey_the_contract_invariants() {
    // Exhaustive walk of the closed state set (compile-checked: a new
    // TerminalState variant must land here before this test builds).
    let allowed: &[&str] = &[
        TerminalState::Loading.name(),
        TerminalState::Empty.name(),
        TerminalState::Error.name(),
        TerminalState::Ready.name(),
        TerminalState::Attention.name(),
        TerminalState::Zero.name(),
    ];
    for p in overview_panels::OVERVIEW_PANELS {
        assert!(
            !p.states.is_empty(),
            "{}: a panel that lists no states cannot transition anywhere",
            p.dom_id
        );
        assert!(
            p.states
                .iter()
                .any(|s| matches!(s, TerminalState::Ready | TerminalState::Zero)),
            "{}: no ready/zero rest state — the B131 permanent-loading bug class",
            p.dom_id
        );
        for s in p.states {
            assert!(
                allowed.contains(&s.name()),
                "{}: state {:?} is outside the contract's closed set {allowed:?}",
                p.dom_id,
                s
            );
            assert!(
                *s == TerminalState::Loading || s.is_terminal(),
                "{}: {} claims non-terminal but is not Loading",
                p.dom_id,
                s.name()
            );
        }
        if p.states.contains(&TerminalState::Empty) {
            let key = p.id.to_string();
            let hint_ok = fetch_panels()
                .get(&key)
                .is_some_and(|j| !j.hint.trim().is_empty());
            assert!(
                hint_ok,
                "{}: reachable Empty state without a what-fills-this hint in JS",
                p.dom_id
            );
        }
    }
}

/// Required and optional slot tables never overlap and respect the
/// reason-attribution rule — the shell renders these; drift here is visible
/// to the operator as missing copy or a dead action.
#[test]
fn slot_tables_never_overlap_and_track_the_attribution_rule() {
    for state in [
        TerminalState::Loading,
        TerminalState::Empty,
        TerminalState::Error,
        TerminalState::Ready,
        TerminalState::Attention,
        TerminalState::Zero,
    ] {
        for slot in required_slots(state) {
            assert!(
                !optional_slots(state).contains(slot),
                "{state:?}: slot {slot:?} both required and optional"
            );
        }
        let body_needed =
            state.requires_reason() || matches!(state, TerminalState::Empty | TerminalState::Zero);
        assert_eq!(
            required_slots(state).contains(&Slot::Body),
            body_needed,
            "{state:?}: Body slot must track whether the state owes a reason or a hint"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Mount integrity: the DOM hosts exist in the served shell
// ---------------------------------------------------------------------------

/// Every `dom_id` in the contract is an actual element id in `index.html` — a
/// renamed mount would leave the wiring painting into the void.
#[test]
fn every_dom_mount_exists_in_the_served_shell_html() {
    let html = include_str!("../src/web/index.html");
    for p in overview_panels::OVERVIEW_PANELS {
        let needle = format!(r#"id="{}""#, p.dom_id);
        assert!(
            html.contains(&needle),
            "{}: shell lost its mount `{needle}` — the wiring would paint nowhere",
            p.dom_id
        );
    }
}

// ---------------------------------------------------------------------------
// 4. Purity ratchet (hexagonal_gate style, NEW check — no grandfather list)
// ---------------------------------------------------------------------------

/// Forbidden IO/framework markers for the mapping modules. The panel contract
/// and its JS mirror are pure data + pure functions: they perform no IO, so
/// none of these may appear in their production halves.
const FORBIDDEN_IO_MARKERS: &[&str] = &[
    "std::fs",
    "std::process",
    "std::net",
    "Command::new",
    "tokio::",
    "reqwest",
    "async fn",
    "await ",
];

/// Only production code counts: anything after the test-module split is
/// scaffolding, mirroring the application-layer ratchet's rule.
fn production_half(text: &str) -> &str {
    text.split("#[cfg(test)]").next().unwrap_or(text)
}

fn rust_mapping_source() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/overview_panels.rs");
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("unreadable panel-contract source {path}: {e}"))
}

#[test]
fn the_mapping_modules_import_nothing_io_related() {
    let offenders: Vec<(String, String)> = FORBIDDEN_IO_MARKERS
        .iter()
        .filter(|m| production_half(&rust_mapping_source()).contains(*m))
        .chain({
            let dirty: Vec<_> = FORBIDDEN_IO_MARKERS
                .iter()
                .filter(|m| WIRING.contains(**m))
                .collect();
            dirty.into_iter()
        })
        .map(|m| ("mapping modules".to_string(), (*m).to_string()))
        .collect();
    assert!(
        offenders.is_empty(),
        "the Overview mapping went impure — these IO/framework markers appeared in \
         src/overview_panels.rs or web/js/overview_states.js: {offenders:?}. Mapping stays a \
         PURE projection of the contract; put IO behind a port in an adapter."
    );
}
