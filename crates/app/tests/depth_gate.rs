//! The depth ratchet (CXA-B188b): every element the UI depth registry
//! declares is deep enough, and a newly shallow one fails the gate with a
//! message naming the element and its missing depth elements.
//!
//! Mirrors `hexagonal_gate.rs` exactly: ALL IO (reading the registry from the
//! repo) is confined to this test harness; the decision itself is the PURE
//! `coxagent_domain::depth::evaluate` from CXA-B188a. The gate fails on any
//! violation that is not explicitly grandfathered below.
//!
//! Grandfather policy — THE LIST MAY ONLY SHRINK. Entries are for elements
//! that predate the depth bar (CXA-B188); fixing them is deliberately out of
//! this ticket's scope. When an element is made compliant, REMOVE its entry
//! in the same PR. Never add an entry: a new element must ship deep or fail.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use coxagent_domain::depth::{evaluate, DepthElement, DepthElementKind};
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// The grandfather list — pre-existing shallow elements (only shrinks)
// ---------------------------------------------------------------------------

/// `(element id, why it is still grandfathered)`. Every entry must name a
/// real reason; when the element is deepened the entry goes with the PR.
const GRANDFATHERED: &[(&str, &str)] = &[
    // Alerts history is a flat render of the webhook spool (CXA-F235): it has
    // an implicit newest-first order and an empty state, but no filter or
    // pagination — deepening it is a UI ticket, not a gate ticket.
    (
        "alerts.history",
        "predates the depth bar (CXA-F235); no filter/pagination yet",
    ),
];

/// The UI depth registry: every element this repo renders above the fold, the
/// depth-bar kind it is, and the depth elements it already carries. This is
/// the one place a shipped element is declared — the gate refuses an unknown
/// kind so a typo can never look compliant.
fn depth_registry() -> Vec<DepthElement> {
    let list = |id: &str, has: &[&str]| DepthElement {
        id: id.to_owned(),
        kind: DepthElementKind::List,
        has: has.iter().map(|s| (*s).to_owned()).collect(),
    };
    let number = |id: &str, has: &[&str]| DepthElement {
        id: id.to_owned(),
        kind: DepthElementKind::Number,
        has: has.iter().map(|s| (*s).to_owned()).collect(),
    };
    vec![
        // Overview KPI tiles (kpis.js, CXA-F360): window = the 14-day spark,
        // delta = signed vs prior 14d, zero-state hint = the vhint swap.
        number("overview.kpis", &["window", "delta", "zero_state_hint"]),
        // The hybrid-team inbox (inbox.js): filter chips (all/assigned to me),
        // a documented escalation-first sort, per-chip counts, the parked
        // collapse + empty states, and the on-hold collapse for volume.
        list(
            "inbox.queue",
            &["filter", "sort", "counts", "pagination", "empty_state"],
        ),
        // Alerts delivery history (alerts.js) — grandfathered, see above.
        list("alerts.history", &["sort", "empty_state"]),
    ]
}

// ---------------------------------------------------------------------------
// The gate, run against the real tree (IO stays inside the harness)
// ---------------------------------------------------------------------------

fn registry_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../presentation/src/depth_registry.rs")
        .canonicalize()
        .expect("the depth registry file must exist next to the UI it describes")
}

/// The failure the gate reports: every un-grandfathered finding, one
/// actionable line each, naming the element and its missing depth elements.
fn why_depth_not_met(findings: &[&coxagent_domain::depth::DepthViolation]) -> Option<String> {
    if findings.is_empty() {
        return None;
    }
    let lines: Vec<String> = findings.iter().map(|f| f.message()).collect();
    Some(format!(
        "the depth bar is violated by {} element(s):\n{}\n\
         fix the element(s) or, only if they predate CXA-B188, shrink-to-fit \
         via the grandfather list in depth_gate.rs (the list may only shrink)",
        findings.len(),
        lines.join("\n")
    ))
}

#[test]
fn every_registered_element_meets_the_depth_bar() {
    // IO, confined to the harness: read the registry source the gate pins.
    let src = fs::read_to_string(registry_file()).expect("registry must be readable");
    assert!(
        src.contains("pub fn depth_registry()"),
        "the pinned registry file must still declare `pub fn depth_registry()`"
    );

    // The decision is the PURE checker from CXA-B188a.
    let findings = evaluate(&depth_registry());
    let ungrandfathered: Vec<_> = findings
        .iter()
        .filter(|f| !GRANDFATHERED.iter().any(|(id, _)| id == &f.element))
        .collect();

    if let Some(why) = why_depth_not_met(&ungrandfathered) {
        panic!("{why}");
    }
    // Nothing silently passes: a grandfather entry must name a finding that
    // actually exists, else the list is lying about what is still shallow.
    for (id, _) in GRANDFATHERED {
        assert!(
            findings.iter().any(|f| &f.element == id),
            "grandfather entry `{id}` matches no current violation — remove it \
             (the list may only shrink)"
        );
    }
}

// ---------------------------------------------------------------------------
// Negative test: a seeded violation must fail, loudly and actionably
// ---------------------------------------------------------------------------

#[test]
fn the_gate_fails_on_a_seeded_violation_and_names_the_element() {
    // Seed: a number stripped of its window/delta/zero-state — exactly what a
    // drive-by "make the tile simpler" edit would produce.
    let mut seeded = depth_registry();
    let victim = seeded
        .iter_mut()
        .find(|e| e.id == "overview.kpis")
        .expect("the seed targets a registered element");
    victim.has.clear();

    let findings = evaluate(&seeded);
    let ungrandfathered: Vec<_> = findings
        .iter()
        .filter(|f| !GRANDFATHERED.iter().any(|(id, _)| id == &f.element))
        .collect();

    let why = why_depth_not_met(&ungrandfathered).expect("a stripped element must fail the gate");
    assert!(
        why.contains("overview.kpis"),
        "the message names the element: {why}"
    );
    for missing in ["window", "delta", "zero_state_hint"] {
        assert!(
            why.contains(missing),
            "the message names the missing depth element `{missing}`: {why}"
        );
    }
    assert!(
        why.contains("grandfather list"),
        "the message says where the escape hatch is NOT to be used: {why}"
    );
    // The seed is in-memory only — the registry on disk is untouched.
    let on_disk = fs::read_to_string(registry_file()).unwrap();
    assert!(
        on_disk.contains("window"),
        "the negative test must not mutate the real registry"
    );
}
