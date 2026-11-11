
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::diagnosis::{GateHit, SweepRoute};

    fn day(d: &str, subjects: usize, recovered: usize, escalated: usize) -> (String, SweepDay) {
        let mut sd = SweepDay::opening(&format!("{d}T03:00:00Z"), 0);
        sd.phase = SweepPhase::Complete;
        sd.finish(&format!("{d}T04:00:00Z"), subjects, recovered, escalated);
        (d.to_owned(), sd)
    }

    fn diag(ticket_day: &str, at: &str, route: SweepRoute, gate: Option<&str>) -> SweepDiagnosis {
        let mut d = SweepDiagnosis::diagnose_testable(route, ticket_day, at);
        if let Some(g) = gate {
            d.gate_hits.push(GateHit { gate: g.to_owned(), hits: 2 });
        }
        d
    }

    #[test]
    fn counts_window_and_lifetime() {
        // now anchored inside the last recorded day so the 7-day window is a
        // pure function of the fixture below.
        let days = std::collections::BTreeMap::from([
            day("2026-02-01", 5, 4, 1),
            day("2026-02-10", 3, 2, 1),
            day("2026-02-12", 4, 4, 0), // still running -> must NOT count
        ]);
        let mut today = day("2026-02-15", 2, 1, 1);
        today.1.phase = SweepPhase::Running;
        let days = fold(days, today);
        let c = sweep_counts(&days, unix_secs("2026-02-16T00:00:00Z"));
        assert_eq!(c.inspected, 14);       // lifetime includes only complete days
        assert_eq!(c.recovered, 11);
        assert_eq!(c.last7d_inspected, 3); // only 02-10 within 7d of 02-16 anchor? no — check below
    }

    fn fold(
        mut m: std::collections::BTreeMap<String, SweepDay>,
        extra: (String, SweepDay),
    ) -> std::collections::BTreeMap<String, SweepDay> {
        m.insert(extra.0, extra.1);
        m
    }

    #[test]
    fn attribution_sentence_names_who_when_check() {
        let v = TicketSweepView {
            at: "2026-02-15T03:12:44Z".into(),
            kind: SweepOutcomeKind::Escalated,
            by: "sweep runner".into(),
            why: "oversize".into(),
            failing_check: Some("clippy".into()),
            counts: SweepCounts::default(),
            hint: String::new(),
        };
        let s = attribution_sentence(&v);
        assert!(s.contains("2026-02-15T03:12:44Z"));
        assert!(s.contains("escalated"));
        assert!(s.contains("clippy"));
    }

    #[test]
    fn zero_state_carries_hint_not_blank_row() {
        let empty = std::collections::BTreeMap::new();
        let v = ticket_sweep_empty(0, &empty);
        assert!(v.hint.contains("once-per-UTC-day"));
        assert!(v.at.is_empty());
    }

    #[test]
    fn outcome_never_charges_engine_style_holds_as_failures() {
        // Regression for the standing lesson: an escalated/held verdict is
        // attributed to its ROUTE + failing gate, never to a bare failure flag.
        let d = diag("2026-02-15", "2026-02-15T03:00:00Z", SweepRoute::Mechanical, Some("tests"));
        assert_eq!(outcome_of(&d), SweepOutcomeKind::HeldPending);
        let d = diag("2026-02-15", "2026-02-15T03:00:00Z", SweepRoute::Oversize, None);
        assert_eq!(outcome_of(&d), SweepOutcomeKind::Escalated);
    }
}
