//! Above-the-fold panel-inventory drift (CXA-B301, part 2/3 of CXA-B240.2).
//!
//! The PURE comparison between the frozen [`PanelInventoryEntry`] list
//! (committed in `crates/presentation/__inventories__/above_the_fold_panels.golden`)
//! and whatever the caller observed live. Every drift class gets a named
//! finding carrying WHAT changed plus the expected (frozen) versus actual
//! (live) values, so gate output reads like a sentence instead of a boolean.
//!
//! No IO, no fetching: the caller supplies BOTH sides. Acquiring the live
//! side is the next subtask's port adapter; nothing here ever touches
//! fs/process/render frameworks.

use crate::panel_inventory_entry::PanelInventoryEntry;

/// The five observable drift classes: a frozen panel went missing, a live
/// panel is not in the freeze, a shared panel sits at a different slot, or
/// a shared panel mutated its identity fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftClass {
    /// Frozen but not rendered anymore ("missing panel").
    Missing,
    /// Rendered live but absent from the freeze ("unexpected panel").
    Unexpected,
    /// Present on both sides but reordered.
    Reordered,
    /// Present on both sides but one of its identity fields differs.
    Mutated,
}

impl DriftClass {
    /// Lowercase tag used verbatim in formatted lines.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unexpected => "unexpected",
            Self::Reordered => "reordered-slot",
            Self::Mutated => "mutated",
        }
    }
}

/// One drift finding: WHICH panel, WHAT changed, and the expected-vs-actual
/// values whenever both sides have something to say about that aspect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftFinding {
    pub class: DriftClass,
    /// Stable panel id (`drain`, `kpis`, …).
    pub panel_id: String,
    /// The aspect that drifted: `"presence"` for missing/unexpected,
    /// `"position"` for reorder, else the exact field name
    /// (`"component_name"`, `"visibility_rules_hash"`).
    pub what: &'static str,
    /// What the frozen inventory says (absent for additions).
    pub expected: Option<String>,
    /// What the live observation shows (absent for removals).
    pub actual: Option<String>,
}

impl DriftFinding {
    /// The single human-readable line for this finding (no trailing newline).
    #[must_use = "gate output is built from these lines"]
    pub fn line(&self) -> String {
        fn show(v: Option<&str>) -> &str { v.unwrap_or("(none)") }
        match self.class {
            DriftClass::Missing => format!(
                "MISSING  {}: expected {}, got {}",
                self.panel_id,
                show(self.expected.as_deref()),
                show(self.actual.as_deref())
            ),
            DriftClass::Unexpected => format!(
                "UNEXPECTED  {}: expected {}, got {}",
                self.panel_id,
                show(self.expected.as_deref()),
                show(self.actual.as_deref())
            ),
            _ => format!(
                "{:<10} {}: {} — expected `{}`, got `{}`",
                self.class.tag(),
                self.panel_id,
                self.what,
                show(self.expected.as_deref()),
                show(self.actual.as_deref())
            ),
        }
    }
}

/// The structured result of comparing frozen vs live inventory. Buckets keep
/// the four drift categories apart so callers can assert (and gates can
/// weight) each class independently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriftReport {
    /// Panels the freeze knows that the live view no longer renders.
    pub removed: Vec<DriftFinding>,
    /// Panels rendered live that the freeze never heard of.
    pub added: Vec<DriftFinding>,
    /// Shared panels whose paint order changed.
    pub moved: Vec<DriftFinding>,
    /// Shared panels whose `component_name` or `visibility_rules_hash`
    /// changed without leaving the layout.
    pub mutated: Vec<DriftFinding>,
}

impl DriftReport {
    /// True when every bucket is empty — the two inventories agree exactly.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.removed.is_empty()
            && self.added.is_empty()
            && self.moved.is_empty()
            && self.mutated.is_empty()
    }

    /// Total number of findings across all classes.
    #[must_use]
    pub fn total(&self) -> usize {
        self.removed.len() + self.added.len() + self.moved.len() + self.mutated.len()
    }

    /// All findings, frozen-first in stable order (removed, added, moved,
    /// mutated) — the sequence the formatter walks.
    pub fn findings(&self) -> impl Iterator<Item = &DriftFinding> {
        self.removed
            .iter()
            .chain(&self.added)
            .chain(&self.moved)
            .chain(&self.mutated)
    }
}

/// Compare the frozen inventory against the live observation. Pure and total:
/// equal sequences yield an empty report; every missing, unexpected,
/// reordered or mutated panel gets exactly one finding per affected aspect.
///
/// Moves are judged on RELATIVE order of panels both sides still agree on, so
/// an insertion elsewhere does not read as every neighbour moving. Slots in
/// expected/actual are 1-based ("slot 3") to match human counting in the UI.
#[must_use]
pub fn panel_inventory_drift(
    frozen: &[PanelInventoryEntry],
    live: &[PanelInventoryEntry],
) -> DriftReport {
    let mut report = DriftReport::default();

    // Removals & mutations walk the frozen order for determinism.
    for (fi, f) in frozen.iter().enumerate() {
        match live.iter().enumerate().find(|(_, l)| l.panel_id == f.panel_id) {
            None => report.removed.push(DriftFinding {
                class: DriftClass::Missing,
                panel_id: f.panel_id.clone(),
                what: "presence",
                expected: Some(format!("rendered at slot {}", fi + 1)),
                actual: None,
            }),
            Some((li, l)) => {
                if f.component_name != l.component_name {
                    report.mutated.push(DriftFinding {
                        class: DriftClass::Mutated,
                        panel_id: f.panel_id.clone(),
                        what: "component_name",
                        expected: Some(f.component_name.clone()),
                        actual: Some(l.component_name.clone()),
                    });
                }
                if f.visibility_rules_hash != l.visibility_rules_hash {
                    report.mutated.push(DriftFinding {
                        class: DriftClass::Mutated,
                        panel_id: f.panel_id.clone(),
                        what: "visibility_rules_hash",
                        expected: Some(f.visibility_rules_hash.clone()),
                        actual: Some(l.visibility_rules_hash.clone()),
                    });
                }
                if fi != li {
                    report.moved.push(DriftFinding {
                        class: DriftClass::Reordered,
                        panel_id: f.panel_id.clone(),
                        what: "position",
                        expected: Some(format!("slot {}", fi + 1)),
                        actual: Some(format!("slot {}", li + 1)),
                    });
                }
            }
        }
    }

    // Additions walk the live order, skipping everything the freeze matched.
    for (li, l) in live.iter().enumerate() {
        if !frozen.iter().any(|f| f.panel_id == l.panel_id) {
            report.added.push(DriftFinding {
                class: DriftClass::Unexpected,
                panel_id: l.panel_id.clone(),
                what: "presence",
                expected: None,
                actual: Some(format!("rendered at slot {}", li + 1)),
            });
        }
    }

    report
}

/// Human-readable rendering of a [`DriftReport`] for gate output. Kept
/// strictly separate from the diff logic: the comparator decides WHAT drifted,
/// this function alone decides how a human reads it. An empty/clean report
/// still prints a positive confirmation instead of blank lines.
#[must_use]
pub fn render_drift_report(report: &DriftReport) -> String {
    if report.is_clean() {
        return "above-the-fold panel inventory matches the frozen snapshot".into();
    }
    let mut out = format!(
        "above-the-fold panel inventory DRIFTED ({} finding{})\n",
        report.total(),
        if report.total() == 1 { "" } else { "s" }
    );
    for f in report.findings() {
        out.push_str("  ");
        out.push_str(&f.line());
        out.push('\n');
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_A: &str = "a93965831c534267";
    const HASH_B: &str = "77ee72e7d6fe454f";

    fn entry(panel_id: &str, slot: u32, component: &str, hash: &str) -> PanelInventoryEntry {
        PanelInventoryEntry::try_new(panel_id, "#/overview", slot, component, hash)
            .expect("valid fixture entry")
    }

    fn frozen_trio() -> Vec<PanelInventoryEntry> {
        vec![
            entry("drain", 0, "shell.js drainBanner()", HASH_A),
            entry("alerts", 1, "core.js alertsHtml()", HASH_A),
            entry("kpis", 2, "kpis.js overviewKpis()", HASH_B),
        ]
    }

    fn live_mirror(frozen: &[PanelInventoryEntry]) -> Vec<PanelInventoryEntry> {
        frozen.to_vec()
    }

    #[test]
    fn identical_inputs_yield_an_empty_report_across_every_bucket() {
        let frozen = frozen_trio();
        let r = panel_inventory_drift(&frozen, &live_mirror(&frozen));
        assert!(r.is_clean(), "equal inventories cannot drift");
        assert_eq!(r.total(), 0);
        assert!(r.removed.is_empty() && r.added.is_empty() && r.moved.is_empty() && r.mutated.is_empty());
    }

    #[test]
    fn a_missing_frozen_panel_is_reported_with_its_expected_slot_and_no_actual() {
        let frozen = frozen_trio();
        let live: Vec<_> = frozen.iter().filter(|e| e.panel_id != "kpis").cloned().collect();
        let r = panel_inventory_drift(&frozen, &live);
        assert_eq!(r.removed.len(), 1);
        let f = &r.removed[0];
        assert_eq!(f.panel_id, "kpis");
        assert_eq!(f.what, "presence");
        assert_eq!(f.expected.as_deref(), Some("rendered at slot 3"));
        assert_eq!(f.actual, None);
        assert!(r.added.is_empty() && r.moved.is_empty() && r.mutated.is_empty());
    }

    #[test]
    fn an_unexpected_live_panel_is_reported_with_a_slot_and_no_expectation() {
        let frozen = frozen_trio();
        let mut live = live_mirror(&frozen);
        live.push(entry("surprise-widget", 3, "who.js paintsIt()", HASH_B));
        let r = panel_inventory_drift(&frozen, &live);
        assert_eq!(r.added.len(), 1);
        let f = &r.added[0];
        assert_eq!(f.panel_id, "surprise-widget");
        assert_eq!(f.what, "presence");
        assert_eq!(f.expected, None);
        assert_eq!(f.actual.as_deref(), Some("rendered at slot 4"));
        assert!(r.removed.is_empty() && r.moved.is_empty() && r.mutated.is_empty());
    }

    #[test]
    fn a_reordered_panel_reports_reorder_without_add_remove_noise() {
        let frozen = frozen_trio();
        let mut live = live_mirror(&frozen);
        live.swap(0, 1);
        let r = panel_inventory_drift(&frozen, &live);
        let moved: Vec<(String, u32, u32)> = r
            .moved
            .iter()
            .map(|f| {
                (
                    f.panel_id.clone(),
                    f.expected.clone().unwrap_or_default(),
                    f.actual.clone().unwrap_or_default(),
                )
            })
            .map(|(p, e, a)| {
                (
                    p,
                    e.trim_start_matches("slot ").parse().unwrap(),
                    a.trim_start_matches("slot ").parse().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            moved,
            vec![("drain".into(), 1, 2), ("alerts".into(), 2, 1)],
            "both swapped panels move, nobody disappears"
        );
        assert!(r.removed.is_empty() && r.added.is_empty() && r.mutated.is_empty());
    }

    #[test]
    fn changing_component_name_is_a_mutation_with_expected_vs_actual_values() {
        let frozen = frozen_trio();
        let live = vec![
            frozen[0].clone(),
            entry("alerts", 1, "charts.js alertsGraph()", HASH_A),
            frozen[2].clone(),
        ];
        let r = panel_inventory_drift(&frozen, &live);
        assert_eq!(r.mutated.len(), 1);
        let f = &r.mutated[0];
        assert_eq!(f.panel_id, "alerts");
        assert_eq!(f.what, "component_name");
        assert_eq!(f.expected.as_deref(), Some("core.js alertsHtml()"));
        assert_eq!(f.actual.as_deref(), Some("charts.js alertsGraph()"));
        assert!(r.added.is_empty() && r.removed.is_empty() && r.moved.is_empty());
    }

    #[test]
    fn changing_visibility_rules_hash_is_a_mutation_with_expected_vs_actual_values() {
        let frozen = frozen_trio();
        let live = vec![
            frozen[0].clone(),
            frozen[1].clone(),
            entry("kpis", 2, "kpis.js overviewKpis()", HASH_A),
        ];
        let r = panel_inventory_drift(&frozen, &live);
        assert_eq!(r.mutated.len(), 1);
        let f = &r.mutated[0];
        assert_eq!(f.panel_id, "kpis");
        assert_eq!(f.what, "visibility_rules_hash");
        assert_eq!(f.expected.as_deref(), Some(HASH_B));
        assert_eq!(f.actual.as_deref(), Some(HASH_A));
    }

    #[test]
    fn an_insertion_shifts_every_survivor_slot_and_the_report_says_so() {
        // Slots are absolute paint positions: inserting at the top genuinely
        // moves every survivor down one slot, and each shift is a finding.
        let frozen = frozen_trio();
        let mut live = live_mirror(&frozen);
        live.insert(0, entry("banner", 0, "top.js banner()", HASH_A));
        let r = panel_inventory_drift(&frozen, &live);
        assert_eq!(r.moved.len(), 3, "all three survivors shifted one slot: {:?}", r.moved);
        assert_eq!(r.added.len(), 1);
        assert_eq!(r.added[0].panel_id, "banner");
    }

    #[test]
    fn mixed_drift_lands_in_the_right_buckets_at_once() {
        let frozen = frozen_trio();
        let live = vec![
            entry("drain", 0, "shell2.js drainBanner()", HASH_A), // mutated component
            entry("kpis", 1, "kpis.js overviewKpis()", HASH_B),   // reordered (was slot 3)
            entry("guest", 2, "extra.js guest()", HASH_A),        // added
            // "alerts" dropped → missing
        ];
        let r = panel_inventory_drift(&frozen, &live);
        assert_eq!(r.removed.len(), 1);
        assert_eq!(r.removed[0].panel_id, "alerts");
        assert_eq!(r.added.len(), 1);
        assert_eq!(r.added[0].panel_id, "guest");
        assert_eq!(r.moved.len(), 1);
        assert_eq!(r.moved[0].panel_id, "kpis");
        assert_eq!(r.mutated.len(), 1);
        assert_eq!(r.mutated[0].panel_id, "drain");
        assert_eq!(r.total(), 4);
        assert!(!r.is_clean());
    }

    #[test]
    fn findings_iterator_walks_all_four_buckets_in_stable_order() {
        let frozen = frozen_trio();
        let live = vec![
            entry("drain", 0, "x.js a()", HASH_A),
            entry("kpis", 1, "kpis.js overviewKpis()", HASH_B),
        ];
        let r = panel_inventory_drift(&frozen, &live);
        let seq: Vec<&'static str> = r.findings().map(|f| f.what).collect();
        assert_eq!(seq, vec!["presence", "position", "component_name"]);
    }

    #[test]
    fn drift_class_tags_are_stable_gate_vocabulary() {
        assert_eq!(DriftClass::Missing.tag(), "missing");
        assert_eq!(DriftClass::Unexpected.tag(), "unexpected");
        assert_eq!(DriftClass::Reordered.tag(), "reordered-slot");
        assert_eq!(DriftClass::Mutated.tag(), "mutated");
    }
}

#[cfg(test)]
mod fmt_tests {
    //! Formatting assertions, deliberately SEPARATE from the diff assertions:
    //! the comparator decides what drifted; only these tests pin how it reads.

    use super::*;

    fn frozen_pair() -> Vec<PanelInventoryEntry> {
        vec![
            PanelInventoryEntry::try_new(
                "drain",
                "#/overview",
                0,
                "shell.js drainBanner()",
                "a93965831c534267",
            )
            .expect("fixture"),
            PanelInventoryEntry::try_new(
                "alerts",
                "#/overview",
                1,
                "core.js alertsHtml()",
                "77ee72e7d6fe454f",
            )
            .expect("fixture"),
        ]
    }

    #[test]
    fn a_clean_report_says_so_instead_of_printing_nothing() {
        let frozen = frozen_pair();
        let r = panel_inventory_drift(&frozen, &frozen);
        assert_eq!(
            render_drift_report(&r),
            "above-the-fold panel inventory matches the frozen snapshot"
        );
    }

    #[test]
    fn a_missing_panel_names_panel_expected_value_and_absence() {
        let frozen = frozen_pair();
        let live = vec![frozen[0].clone()];
        let out = render_drift_report(&panel_inventory_drift(&frozen, &live));
        assert!(out.contains("DRIFTED (1 finding)"), "{out}");
        assert!(out.contains("MISSING  alerts"), "{out}");
        assert!(out.contains("rendered at slot 2"), "{out}");
        assert!(out.contains("(none)"), "{out}");
    }

    #[test]
    fn an_added_panel_states_there_was_no_freeze_entry_and_where_it_rendered() {
        let frozen = frozen_pair();
        let mut live = frozen.clone();
        live.push(
            PanelInventoryEntry::try_new(
                "guest",
                "#/overview",
                2,
                "extra.js guest()",
                "a93965831c534267",
            )
            .expect("fixture"),
        );
        let out = render_drift_report(&panel_inventory_drift(&frozen, &live));
        assert!(out.contains("UNEXPECTED  guest"), "{out}");
        assert!(out.contains("rendered at slot 3"), "{out}");
    }

    #[test]
    fn a_moved_panel_shows_slot_from_and_slot_to() {
        let frozen = frozen_pair();
        let live = vec![frozen[1].clone(), frozen[0].clone()];
        let out = render_drift_report(&panel_inventory_drift(&frozen, &live));
        assert!(out.contains("reordered-slot"), "{out}");
        assert!(out.contains("expected `slot 2`, got `slot 1`"), "{out}");
    }

    #[test]
    fn a_mutated_panel_shows_both_field_values_verbatim() {
        let frozen = frozen_pair();
        let live = vec![
            frozen[0].clone(),
            PanelInventoryEntry::try_new(
                "alerts",
                "#/overview",
                1,
                "charts.js alertsGraph()",
                "deadbeef00000001",
            )
            .expect("fixture"),
        ];
        let out = render_drift_report(&panel_inventory_drift(&frozen, &live));
        assert!(out.contains("mutated"), "{out}");
        assert!(out.contains("component_name — expected `core.js alertsHtml()`, got `charts.js alertsGraph()`"), "{out}");
        assert!(out.contains("visibility_rules_hash — expected `77ee72e7d6fe454f`, got `deadbeef00000001`"), "{out}");
    }

    #[test]
    fn pluralisation_and_line_per_finding_for_multi_drift() {
        let frozen = frozen_pair();
        let live = vec![
            PanelInventoryEntry::try_new(
                "drain",
                "#/overview",
                0,
                "other.js drained()",
                "a93965831c534267",
            )
            .expect("fixture"),
        ];
        let r = panel_inventory_drift(&frozen, &live);
        assert_eq!(r.total(), 2); // alerts missing, drain mutated (order holds)
        let out = render_drift_report(&r);
        assert!(out.contains("2 findings"), "{out}");
        assert_eq!(out.lines().count(), 3, "header + two finding lines: {out}");
    }
}
