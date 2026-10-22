//! The UI depth registry (CXA-B188b): the ONE place a shipped UI element
//! declares which depth-bar elements it carries, so the depth gate
//! (`crates/app/tests/depth_gate.rs`) can enforce the bar mechanically.
//!
//! The registry is data, not logic — the decision is the pure checker in
//! `coxagent_domain::depth`. Adding a surface here means declaring it with
//! every depth element its kind requires; elements that predate the bar are
//! grandfathered in the GATE, never here, and the grandfather list may only
//! shrink.
//!
//! This file is Rust so the gate can read and pin it; the ids it declares name
//! the JS renderers in `src/web/js/` that implement each element.

use coxagent_domain::depth::{DepthElement, DepthElementKind};

/// Every element this repo renders above the fold. New surfaces MUST be
/// declared here (the gate fails an undeclared-compliance drift only for
/// listed ids, so a missing declaration is a review catch, not a silent pass).
#[must_use]
pub fn depth_registry() -> Vec<DepthElement> {
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
        // Overview KPI tiles (`src/web/js/kpis.js`, CXA-F360): the number is
        // shown with its window (14-day spark, "vs prior 14d"), a signed
        // delta, and a zero-state hint when a tile sits at 0 with no history.
        number("overview.kpis", &["window", "delta", "zero_state_hint"]),
        // The hybrid-team inbox queue (`src/web/js/inbox.js`): filter chips
        // (All / assigned to me, persisted), a documented escalation-first
        // sort, per-chip counts, the on-hold collapse that keeps a hundred
        // parked tickets to one card, and an explicit empty state.
        list(
            "inbox.queue",
            &["filter", "sort", "counts", "pagination", "empty_state"],
        ),
        // Alert delivery history (`src/web/js/alerts.js`, CXA-F235) — the
        // webhook spool rendered newest-first with an empty state; its depth
        // gaps are grandfathered in the gate, not hidden here.
        list("alerts.history", &["sort", "empty_state"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_capability_is_legal_for_its_kind() {
        // A typo'd capability would make a shallow element look compliant —
        // the checker ignores anything its kind does not require.
        for e in depth_registry() {
            for c in &e.has {
                assert!(
                    e.kind.accepts(c),
                    "element `{}` (kind {}) declares `{c}`, which is not a \
                     depth element of its kind",
                    e.id,
                    e.kind.name()
                );
            }
        }
    }

    #[test]
    fn registry_ids_are_namespaced_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for e in depth_registry() {
            assert!(e.id.contains('.'), "`{}` needs a `<area>.<name>` id", e.id);
            assert!(seen.insert(e.id.clone()), "duplicate id `{}`", e.id);
        }
    }
}
