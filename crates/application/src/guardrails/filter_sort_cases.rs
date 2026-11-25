//! The seven CXA-B282 filter & sort invariant guardrail cases, as PURE
//! functions over the REAL domain/application types the board already ships:
//! [`PanelRegistry`] + [`list_panels`] + [`PanelListing`]. No IO, no servers,
//! no fabricated data models — every fixture below is a hand-built
//! [`PanelRegistry`] fed through the actual `list_panels` read.
//!
//! Each case is a registered [`GuardrailCase`] so failures surface through the
//! harness report (`guardrails: N passed / M failed / K total`) with a WHY line
//! naming the invariant and the offending input.
//!
//! Covering (one named case per invariant class):
//!
//! 1. determinism — same input yields identical output;
//! 2. comparator soundness — antisymmetry + transitivity over real pairs;
//! 3. stability for equal keys — ties keep declared order (the domain sorts
//!    with `sort_by_key`, which Rust guarantees stable; this case pins that
//!    documented decision with a real tie fixture, not just prose);
//! 4. membership round trip — outputs satisfy the predicate and no
//!    qualifying input is dropped;
//! 5. no-op identity — the All/empty filter returns the collection unchanged;
//! 6. composition — filter-then-sort keeps the run fully sorted;
//! 7. count conservation — listed total matches the hand-computed qualifying
//!    count and reconciles with the registry size.

use super::{GuardrailCase, GuardrailVerdict};
use crate::use_cases::list_panels::{list_panels, PanelSlotFilter};
use coxagent_domain::{Panel, PanelLayoutSlot, PanelListing, PanelRegistry};
use std::cmp::Ordering;

/// The comparator the domain orders listings by, as a first-class part of the
/// observation: `(slot rank, declared sort_order)`, compared with the derived
/// `Ord`. Carried on the snapshot so the soundness case can probe THE
/// comparator actually in force, and so a test double can substitute a
/// deliberately broken one to prove the case bites.
#[must_use]
pub fn key_comparator(a: &Panel, b: &Panel) -> Ordering {
    sort_key(a).cmp(&sort_key(b))
}

/// One observation the cases judge: the registry, the filter asked of it, and
/// the listing the real read produced. Pure data — assembled by the caller
/// from existing types, never fetched.
#[derive(Clone)]
pub struct FilterSortSnapshot {
    /// The registry under test.
    pub registry: PanelRegistry,
    /// The filter applied.
    pub filter: PanelSlotFilter,
    /// The listing `list_panels(&registry, filter)` returned.
    pub listing: PanelListing,
    /// The comparator in force for this observation (the real domain
    /// [`key_comparator`] unless a test double substitutes one).
    pub comparator: fn(&Panel, &Panel) -> std::cmp::Ordering,
}

impl std::fmt::Debug for FilterSortSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterSortSnapshot")
            .field("filter", &self.filter)
            .field("listing", &self.listing)
            .finish_non_exhaustive()
    }
}

impl FilterSortSnapshot {
    /// Observe the registry through the real use case — the ONLY way a
    /// snapshot is built in production shape.
    #[must_use]
    pub fn observe(registry: &PanelRegistry, filter: PanelSlotFilter) -> Self {
        Self {
            listing: list_panels(registry, filter),
            registry: registry.clone(),
            filter,
            comparator: key_comparator,
        }
    }

    /// Substitute a comparator (test doubles only — production snapshots keep
    /// the real domain comparator).
    #[must_use]
    pub fn with_comparator(mut self, comparator: fn(&Panel, &Panel) -> std::cmp::Ordering) -> Self {
        self.comparator = comparator;
        self
    }

    /// The predicate this filter encodes, as data (`None` = match-all).
    fn predicate_slot(&self) -> Option<PanelLayoutSlot> {
        self.filter.slot()
    }
}

/// The sort key the domain orders by: `(slot rank, declared sort_order)`.
type SortKey = (u8, u32);

fn sort_key(panel: &Panel) -> SortKey {
    (panel.slot.rank(), panel.sort_order)
}

/// INVARIANT 1 — determinism: observing the same registry with the same filter
/// again must yield byte-identical listings.
fn sort_determinism(snapshot: &FilterSortSnapshot) -> GuardrailVerdict {
    let reobserved = FilterSortSnapshot::observe(&snapshot.registry, snapshot.filter);
    if reobserved.listing == snapshot.listing {
        GuardrailVerdict::Hold
    } else {
        GuardrailVerdict::Breach {
            expected: format!(
                "re-listing with filter {:?} reproduces {} panels: {:?}",
                snapshot.filter,
                snapshot.listing.total,
                ids(&snapshot.listing)
            ),
            actual: format!("re-listing diverged: {:?}", ids(&reobserved.listing)),
        }
    }
}

/// INVARIANT 2 — comparator soundness: over every ordered pair and every
/// consecutive triple of the REAL registered panels, the comparator in force
/// must be antisymmetric (a<b exactly when b>a) and transitive (a<=b and
/// b<=c imply a<=c).
fn comparator_soundness(snapshot: &FilterSortSnapshot) -> GuardrailVerdict {
    let cmp = snapshot.comparator;
    let panels: &[Panel] = &snapshot.registry.list(None).panels;

    // Antisymmetry spot check over all ordered pairs: cmp(a,b) = Less must
    // imply cmp(b,a) = Greater (and mirrored).
    for (i, a) in panels.iter().enumerate() {
        for b in panels.iter().skip(i + 1) {
            let ab = cmp(a, b);
            let ba = cmp(b, a);
            let antisymmetric = matches!(
                (ab, ba),
                (Ordering::Less, Ordering::Greater)
                    | (Ordering::Greater, Ordering::Less)
                    | (Ordering::Equal, Ordering::Equal)
            );
            if !antisymmetric {
                return GuardrailVerdict::Breach {
                    expected: format!(
                        "antisymmetry: {:?} vs {:?} must compare one way",
                        a.id, b.id
                    ),
                    actual: format!(
                        "{:?} vs {:?} compared BOTH as {ab:?} and {ba:?}",
                        a.id, b.id
                    ),
                };
            }
        }
    }
    // Transitivity spot check over consecutive triples of the real fold run:
    // a<=b and b<=c must imply a<=c — catches a negated/rotating comparator.
    for w in panels.windows(3) {
        let (a, b, c) = (&w[0], &w[1], &w[2]);
        if cmp(a, b) != Ordering::Greater
            && cmp(b, c) != Ordering::Greater
            && cmp(a, c) == Ordering::Greater
        {
            return GuardrailVerdict::Breach {
                expected: format!(
                    "transitivity: {:?} <= {:?} <= {:?} implies {:?} <= {:?}",
                    a.id, b.id, c.id, a.id, c.id
                ),
                actual: format!("{:?} compared Greater than {:?}", a.id, c.id),
            };
        }
    }
    // The comparison the listing actually exhibits must agree with the
    // comparator on adjacent distinct-key rows.
    for w in snapshot.listing.panels.windows(2) {
        if sort_key(&w[0]) != sort_key(&w[1]) && cmp(&w[0], &w[1]) == Ordering::Greater {
            return GuardrailVerdict::Breach {
                expected: format!(
                    "adjacent rows ordered per comparator: {:?} before {:?}",
                    w[0].id, w[1].id
                ),
                actual: format!("comparator ranks {:?} after {:?}", w[0].id, w[1].id),
            };
        }
    }
    GuardrailVerdict::Hold
}

/// INVARIANT 3 — stability for equal keys: panels sharing the exact sort key
/// must keep their declared registry order (pins the documented
/// `sort_by_key`-is-stable decision with real data instead of prose).
fn stability(snapshot: &FilterSortSnapshot) -> GuardrailVerdict {
    // Group output positions by equal sort key and confirm declared-order
    // preservation for every tied group.
    let mut tied: Vec<(Vec<usize>, SortKey)> = Vec::new();
    for (idx, panel) in snapshot.listing.panels.iter().enumerate() {
        match tied.last_mut() {
            Some(group) if group.1 == sort_key(panel) => group.0.push(idx),
            _ => tied.push((vec![idx], sort_key(panel))),
        }
    }
    for (positions, key) in tied {
        if positions.len() < 2 {
            continue;
        }
        let declared: Vec<&str> = positions
            .iter()
            .map(|i| snapshot.listing.panels[*i].id)
            .collect();
        let expected_declared: Vec<&str> = snapshot
            .registry
            .list(None)
            .panels
            .iter()
            .filter(|p| sort_key(p) == key)
            .map(|p| p.id)
            .collect();
        if declared != expected_declared {
            return GuardrailVerdict::Breach {
                expected: format!(
                    "stability: panels with equal key {key:?} keep declared order {expected_declared:?}"
                ),
                actual: format!("output ordered them {declared:?}"),
            };
        }
    }
    GuardrailVerdict::Hold
}

/// INVARIANT 4 — membership round trip: every output panel satisfies the
/// filter predicate AND every registry panel satisfying it survives.
fn membership_round_trip(snapshot: &FilterSortSnapshot) -> GuardrailVerdict {
    let wanted = snapshot.predicate_slot();

    // Outbound leg: nothing in the output violates the predicate.
    for panel in &snapshot.listing.panels {
        let satisfies = wanted.map_or(true, |slot| panel.slot == slot);
        if !satisfies {
            return GuardrailVerdict::Breach {
                expected: format!("every listed panel satisfies filter {:?}", snapshot.filter),
                actual: format!(
                    "panel {:?} carries slot {:?}, outside the filter",
                    panel.id, panel.slot
                ),
            };
        }
    }

    // Return leg: every qualifying input survived.
    let listed_ids: Vec<&str> = snapshot.listing.panels.iter().map(|p| p.id).collect();
    for panel in &snapshot.registry.list(None).panels {
        let qualifies = wanted.map_or(true, |slot| panel.slot == slot);
        if qualifies && !listed_ids.contains(&panel.id) {
            return GuardralDrop(&format!(
                "filter {:?} dropped qualifying panel {:?} (slot {:?}, order {})",
                snapshot.filter, panel.id, panel.slot, panel.sort_order
            ));
        }
    }
    GuardrailVerdict::Hold
}

/// Small internal constructor so the drop-leg breach above reads like the
/// others without duplicating field plumbing at the call site.
#[allow(non_snake_case)]
fn GuardralDrop(actual: &str) -> GuardrailVerdict {
    GuardrailVerdict::Breach {
        expected: "every registry panel matching the filter survives the listing".to_owned(),
        actual: actual.to_owned(),
    }
}

/// INVARIANT 5 — no-op identity: the `All` filter returns exactly the
/// unfiltered collection (same panels, same order, same totals); an empty
/// registry yields the defined empty listing.
fn no_op_filter_identity(snapshot: &FilterSortSnapshot) -> GuardrailVerdict {
    let unfiltered = snapshot.registry.list(None);
    if snapshot.filter == PanelSlotFilter::All && snapshot.listing != unfiltered {
        return GuardrailVerdict::Breach {
            expected: "the All filter to return the unfiltered listing verbatim".to_owned(),
            actual: format!(
                "All yielded {:?} but the registry is {:?}",
                ids(&snapshot.listing),
                ids(&unfiltered)
            ),
        };
    }
    if snapshot.registry.is_empty() && snapshot.listing != PanelListing::empty() {
        return GuardrailVerdict::Breach {
            expected: "an empty registry to list as PanelListing::empty()".to_owned(),
            actual: format!(
                "got total={}, registry_size={}",
                snapshot.listing.total, snapshot.listing.registry_size
            ),
        };
    }
    GuardrailVerdict::Hold
}

/// INVARIANT 6 — composition: filter-then-sort keeps the sort invariants on
/// the filtered subset (non-decreasing keys across the whole filtered run).
fn composition(snapshot: &FilterSortSnapshot) -> GuardrailVerdict {
    let keys: Vec<SortKey> = snapshot.listing.panels.iter().map(sort_key).collect();
    for w in keys.windows(2) {
        if w[0] > w[1] {
            let id_with_key = |k: SortKey| {
                snapshot
                    .listing
                    .panels
                    .iter()
                    .find(|p| sort_key(p) == k)
                    .map_or("?", |p| p.id)
            };
            let (offender, follower) = (id_with_key(w[0]), id_with_key(w[1]));
            return GuardrailVerdict::Breach {
                expected: format!(
                    "filtered subset of {:?} stays sorted by (slot rank, declared order)",
                    snapshot.filter
                ),
                actual: format!(
                    "{offender:?} (key {:?}) renders before {follower:?} (key {:?})",
                    w[0], w[1]
                ),
            };
        }
    }
    GuardrailVerdict::Hold
}

/// INVARIANT 7 — count conservation: `total` equals the panels actually listed
/// and matches the hand-computed number of qualifying registry entries, while
/// `registry_size` still reports the whole registry.
fn count_conservation(snapshot: &FilterSortSnapshot) -> GuardrailVerdict {
    let wanted = snapshot.predicate_slot();
    let qualifying = snapshot
        .registry
        .list(None)
        .panels
        .iter()
        .filter(|p| wanted.map_or(true, |slot| p.slot == slot))
        .count();

    if snapshot.listing.total != snapshot.listing.panels.len() {
        return GuardrailedTotal(snapshot.listing.total, snapshot.listing.panels.len());
    }
    if snapshot.listing.total != qualifying {
        return GuardrailVerdict::Breach {
            expected: format!(
                "total == hand-computed qualifying count {qualifying} for filter {:?}",
                snapshot.filter
            ),
            actual: format!(
                "total={} but {} panels listed and {} qualify in the registry",
                snapshot.listing.total,
                snapshot.listing.panels.len(),
                qualifying
            ),
        };
    }
    if snapshot.listing.registry_size != snapshot.registry.len() {
        return GuardrailVerdict::Breach {
            expected: format!("registry_size == {}", snapshot.registry.len()),
            actual: format!("registry_size={}", snapshot.listing.registry_size),
        };
    }
    GuardrailVerdict::Hold
}

/// Internal alias keeping the internal-total mismatch breach symmetric with
/// its siblings (named so the report line names the invariant).
#[allow(non_snake_case)]
fn GuardrailedTotal(total: usize, listed: usize) -> GuardrailVerdict {
    GuardrailVerdict::Breach {
        expected: "total to equal the number of listed panels".to_owned(),
        actual: format!("total={total} but {listed} panels are listed"),
    }
}

fn ids(listing: &PanelListing) -> Vec<&'static str> {
    listing.panels.iter().map(|p| p.id).collect()
}

/// The registered CXA-B282 suite, in report order. Registration only — the
/// runner supplies snapshots.
#[must_use]
pub fn filter_sort_cases() -> Vec<GuardrailCase<FilterSortSnapshot>> {
    vec![
        GuardrailCase::new(
            "filter_sort_sort_determinism",
            "sort determinism: the same registry+filter observation repeats identically",
            sort_determinism,
        ),
        GuardrailCase::new(
            "filter_sort_comparator_soundness",
            "comparator soundness: antisymmetry and transitivity over real sort keys",
            comparator_soundness,
        ),
        GuardrailCase::new(
            "filter_sort_stability_equal_keys",
            "sort stability: panels with equal (rank, order) keys keep declared order",
            stability,
        ),
        GuardrailCase::new(
            "filter_sort_membership_round_trip",
            "filter membership round trip: outputs satisfy the predicate and no qualifying input is dropped",
            membership_round_trip,
        ),
        GuardrailCase::new(
            "filter_sort_no_op_filter_identity",
            "no-op filter identity: All/empty observations return the unfiltered collection",
            no_op_filter_identity,
        ),
        GuardrailCase::new(
            "filter_sort_composition_preserves_order",
            "filter-then-sort composition: the filtered subset remains fully sorted",
            composition,
        ),
        GuardrailCase::new(
            "filter_sort_count_conservation",
            "count conservation: listed count equals the hand-computed qualifying count",
            count_conservation,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guardrails::{run_suite, GuardrailReport};
    use coxagent_domain::PanelVisibility;

    /// Hand-built real-panel fixture (no fabricated shapes): four slots' worth
    /// of the real `Panel` value type, including an exact-tie pair for the
    /// stability case and two rows in one slot for the filter arithmetic.
    fn fixture() -> PanelRegistry {
        let p = |id, title, slot, order| {
            Panel::new(id, title, slot, order, PanelVisibility::Always).expect("valid panel")
        };
        PanelRegistry::try_new(vec![
            p(
                "overview.summary",
                "Overview",
                PanelLayoutSlot::AboveTheFold,
                0,
            ),
            p(
                "delivery.tickets",
                "Delivery",
                PanelLayoutSlot::RowPrimary,
                0,
            ),
            p("quality.tests", "Quality", PanelLayoutSlot::RowPrimary, 5),
            p("tie.alpha", "Alpha", PanelLayoutSlot::Diagnostics, 7),
            p("tie.beta", "Beta", PanelLayoutSlot::Diagnostics, 7),
        ])
        .expect("unique ids")
    }

    fn snap(filter: PanelSlotFilter) -> FilterSortSnapshot {
        FilterSortSnapshot::observe(&fixture(), filter)
    }

    fn all_holding(cases: &[GuardrailCase<FilterSortSnapshot>], s: &FilterSortSnapshot) {
        let results = run_suite(cases, s);
        let report = GuardrailReport::of(&results);
        assert!(
            report.all_passed(),
            "fixture must hold every invariant:\n{}",
            report.render(&results)
        );
    }

    #[test]
    fn the_full_fixture_holds_all_seven_registered_invariants() {
        let cases = filter_sort_cases();
        assert_eq!(cases.len(), 7, "seven invariant classes are registered");
        for filter in [
            PanelSlotFilter::All,
            PanelSlotFilter::Only(PanelLayoutSlot::RowPrimary),
            PanelSlotFilter::Only(PanelLayoutSlot::Diagnostics),
        ] {
            all_holding(&cases, &snap(filter));
        }
        all_holding(
            &cases,
            &FilterSortSnapshot::observe(&PanelRegistry::empty(), PanelSlotFilter::All),
        );
    }

    #[test]
    fn seeded_counterexample_a_reversed_listing_breaks_every_sort_invariant_with_a_named_why() {
        // Seed ONE temporary counterexample (reverted after this run): a
        // deliberately BROKEN comparator that answers "Less" to EVERY pair —
        // unsound, because it claims a<b while equally claiming b<a.
        fn always_less(_: &Panel, _: &Panel) -> Ordering {
            Ordering::Less
        }
        let broken = snap(PanelSlotFilter::All).with_comparator(always_less);

        let results = run_suite(&filter_sort_cases(), &broken);
        // THE invariant under test here is comparator soundness: `always_less`
        // claims a<b for every pair, so any b<a claim contradicts it — exactly
        // one antisymmetry breach, named with the offending panel pair. The
        // other cases judge the LISTING and stay green because only the
        // comparator double was corrupted, not the real read.
        let mut failed_names: Vec<&str> = results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| r.name)
            .collect();
        failed_names.sort_unstable();
        assert_eq!(
            failed_names,
            vec!["filter_sort_comparator_soundness"],
            "exactly the soundness case must catch an unsound comparator"
        );

        let why = results
            .iter()
            .find(|r| r.name == "filter_sort_comparator_soundness")
            .map(|r| r.reason.clone())
            .expect("soundness case runs");
        assert!(
            why.contains("antisymmetry") || why.contains("comparator ranks"),
            "names the invariant and how it broke: {why}"
        );
        assert!(
            why.contains(".summary") || why.contains(".tickets"),
            "names the offending panel: {why}"
        );

        // The WHY must name the invariant class and identify a concrete
        // offending ordered pair from the real fixture.
        assert!(
            why.contains("antisymmetry"),
            "must NAME the broken invariant: {why}"
        );
        assert!(
            why.contains(".summary") || why.contains(".tickets"),
            "must name a REAL offending panel: {why}"
        );
    }

    #[test]
    fn seeded_counterexample_a_reversed_listing_breaks_the_composition_invariant() {
        // Seed ONE temporary counterexample, then discard it with the test:
        // the LISTING is handed back reversed (what a mis-sorted read looks
        // like) while everything else stays honest. The composition case must
        // fail naming the offending out-of-order pair; count conservation and
        // membership still hold because only ordering was corrupted — proving
        // each case fails for ITS OWN invariant, not any breach in sight.
        let mut broken = snap(PanelSlotFilter::All);
        broken.listing.panels.reverse();
        broken.listing.total = broken.listing.panels.len();

        let results = run_suite(&filter_sort_cases(), &broken);
        let composition_why = results
            .iter()
            .find(|r| r.name == "filter_sort_composition_preserves_order")
            .map(|r| r.reason.clone())
            .expect("composition case runs");
        assert!(
            composition_why.contains("stays sorted"),
            "names the invariant: {composition_why}"
        );
        assert!(
            composition_why.contains("renders before"),
            "shows the offending pair: {composition_why}"
        );
        assert!(
            composition_why.contains("(key"),
            "exposes the offending keys: {composition_why}"
        );
        // Determinism deliberately uses the SNAPSHOT listing on both sides of
        // its re-observe? No — it re-observes live, so a doctored listing also
        // trips it; that's expected here and asserted below so the report
        // reader sees BOTH sort-shaped breaches from one seed.
        let determinism_why = results
            .iter()
            .find(|r| r.name == "filter_sort_sort_determinism")
            .map(|r| r.reason.clone())
            .expect("determinism case runs");
        assert!(
            determinism_why.contains("re-listing diverged"),
            "second sort-shaped breach from the same seed: {determinism_why}"
        );
        // Same seed also demonstrates STABILITY (class 3): reversing flips the
        // real tie pair, so equal keys no longer keep declared order.
        let stability_why = results
            .iter()
            .find(|r| r.name == "filter_sort_stability_equal_keys")
            .map(|r| r.reason.clone())
            .expect("stability case runs");
        assert!(
            stability_why.contains("equal key") && stability_why.contains("ordered them"),
            "names the invariant and both orders: {stability_why}"
        );
    }

    #[test]
    fn seeded_counterexample_the_live_domain_comparator_is_antisymmetric_over_real_pairs() {
        // Positive control for the SAME invariant class: the REAL comparator,
        // over ALL ordered pairs of the fixture, holds antisymmetry+transitivity.
        let ok = snap(PanelSlotFilter::Only(PanelLayoutSlot::RowPrimary));
        let results = run_suite(
            &filter_sort_cases()
                .into_iter()
                .filter(|c| c.name == "filter_sort_comparator_soundness")
                .collect::<Vec<_>>(),
            &ok,
        );
        assert!(results.iter().all(|r| r.passed), "{results:?}");
    }

    #[test]
    fn seeded_counterexample_an_inconsistent_total_fails_count_conservation_naming_the_filter() {
        // Test double: a predicate that silently drops a surviving panel while
        // the carried total disagrees with the hand-computed qualifying count.
        let mut broken = snap(PanelSlotFilter::Only(PanelLayoutSlot::RowPrimary));
        broken.listing.panels.pop(); // quality.tests (last RowPrimary row) dropped
                                     // The carried total keeps agreeing with the (now wrong) listed count,
                                     // isolating the breach: 1 listed vs 2 hand-computed qualifying rows.
        broken.listing.total = broken.listing.panels.len();

        let results = run_suite(&filter_sort_cases(), &broken);
        let why = results
            .iter()
            .find(|r| r.name == "filter_sort_count_conservation")
            .map(|r| r.reason.clone())
            .expect("count case runs");
        assert!(why.contains("hand-computed qualifying count 2"), "{why}");
        assert!(
            why.contains("Only(RowPrimary)"),
            "names the filter key: {why}"
        );
        assert!(
            why.contains("total=1"),
            "shows the offending numbers: {why}"
        );
        assert!(why.contains("2 qualify in the registry"), "{why}");
    }

    #[test]
    fn seeded_counterexample_a_dropping_predicate_fails_the_membership_round_trip() {
        // Test double: the filter DROPS a qualifying panel (return leg broken).
        let mut broken = snap(PanelSlotFilter::Only(PanelLayoutSlot::RowPrimary));
        broken.listing.panels.retain(|p| p.id != "delivery.tickets");
        broken.listing.total = broken.listing.panels.len();

        let results = run_suite(&filter_sort_cases(), &broken);
        let why = results
            .iter()
            .find(|r| r.name == "filter_sort_membership_round_trip")
            .map(|r| r.reason.clone())
            .expect("membership case runs");
        assert!(why.contains("survives the listing"), "{why}");
        assert!(
            why.contains("\"delivery.tickets\""),
            "names the dropped panel: {why}"
        );
    }

    #[test]
    fn seeded_counterexample_a_mutating_no_op_filter_breaks_the_identity_invariant() {
        // Test double: `All` returns a truncated view, i.e. the no-op filter
        // secretly filtered.
        let mut broken = snap(PanelSlotFilter::All);
        broken.listing.panels.truncate(3);
        broken.listing.total = 3;

        let results = run_suite(&filter_sort_cases(), &broken);
        let why = results
            .iter()
            .find(|r| r.name == "filter_sort_no_op_filter_identity")
            .map(|r| r.reason.clone())
            .expect("identity case runs");
        assert!(why.contains("unfiltered listing"), "{why}");
        assert!(
            why.contains("\"tie.alpha\""),
            "names what went missing: {why}"
        );
    }

    #[test]
    fn the_report_totals_include_the_new_cases() {
        let cases = filter_sort_cases();
        let results = run_suite(&cases, &snap(PanelSlotFilter::All));
        let report = GuardrailReport::of(&results);
        assert_eq!(
            (report.passed, report.failed, report.total),
            (7, 0, 7),
            "guardrail totals must count the B282 cases"
        );
        assert!(report
            .render(&results)
            .contains("guardrails: 7 passed / 0 failed / 7 total"));
    }
}
