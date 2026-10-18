//! The cycle delivery gate (CXA-F232): whether a freshly-scored cycle may be
//! finalized into the authoritative delivery history (`state.cycle_scores`).
//! Split from `metrics` along the same seam as `metrics_burndown` — one
//! cohesive decision, pure over the scorecard inputs, zero IO.

use crate::state::CycleScore;

/// Whether a freshly-scored cycle may be finalized into the authoritative
/// delivery history (`state.cycle_scores`). Only healthy-delivery grades pass:
/// 'A' (shipped work) or 'B' (useful majority with zero errors). An
/// idle-but-clean 'C' — nothing shipped, usefulness below majority — or a
/// churn/incident 'D' is refused: appending either made idle time masquerade
/// as a delivered cycle and inflated the non-shipping share the trend
/// sentinel kept flagging (C at 96% while shipped stayed 0 and five sprints
/// ended ~7% done-in-full).
///
/// Pure over exactly the inputs [`CycleScore::grade_of`] reads — no IO, no
/// side effects — and re-derived from them on every call, so it is idempotent
/// and legacy stores round-trip unchanged (no schema impact; only FUTURE
/// appends are filtered).
#[must_use]
pub fn can_finalize_cycle(
    shipped: u64,
    runs: u64,
    useful: u64,
    incidents: u64,
    errors: u64,
) -> bool {
    !matches!(
        CycleScore::grade_of(shipped, runs, useful, incidents, errors).as_str(),
        "C" | "D"
    )
}

#[cfg(test)]
mod tests {
    use super::can_finalize_cycle;
    use crate::state::CycleScore;

    #[test]
    fn shipped_work_finalizes_the_cycle() {
        // 'A' — anything shipped finalizes, whatever the run count. Churn only
        // demotes a cycle when NOTHING was useful (runs>=4 && useful==0), and
        // an open incident outranks shipping entirely (tested below).
        assert!(can_finalize_cycle(1, 0, 0, 0, 0));
        assert!(can_finalize_cycle(1, 3, 0, 0, 3));
        assert!(can_finalize_cycle(2, 4, 8, 0, 0));
    }

    #[test]
    fn useful_majority_clean_cycle_finalizes() {
        // 'B' boundary: usefulness reaches exactly half the runs, zero errors.
        assert!(can_finalize_cycle(0, 1, 1, 0, 0));
        assert!(can_finalize_cycle(0, 4, 2, 0, 0));
        // One error breaks the 'B' requirement → falls through to 'C': blocked.
        assert!(!can_finalize_cycle(0, 5, 3, 0, 1));
        // Just under the majority bar is idle-but-clean 'C': blocked.
        assert!(!can_finalize_cycle(0, 4, 1, 0, 0));
    }

    #[test]
    fn idle_churn_and_incident_cycles_are_blocked() {
        // 'D' — churn: four-plus runs produced nothing useful.
        assert!(!can_finalize_cycle(0, 4, 0, 0, 0));
        // 'D' — an open incident blocks finalization even with shipped work
        // (grade_of rules incidents ahead of shipping, so the gate must too).
        assert!(!can_finalize_cycle(1, 2, 2, 1, 0));
        assert!(!can_finalize_cycle(0, 0, 0, 2, 0));
        // 'C' — idle-but-clean: some activity, below majority, nothing shipped.
        assert!(!can_finalize_cycle(0, 6, 1, 0, 0));
    }

    #[test]
    fn empty_cycle_is_blocked_and_the_gate_never_disagrees_with_the_grade() {
        // Empty-runs edge: a default/empty scorecard (runs=0) still computes a
        // grade like any other — 'C' — and the gate refuses it.
        assert_eq!(
            CycleScore::grade_of(0, 0, 0, 0, 0),
            "C",
            "the empty edge must grade, not short-circuit"
        );
        assert!(!can_finalize_cycle(0, 0, 0, 0, 0));
        // The gate is DEFINED by the grade boundaries: finalized exactly when
        // the score is 'A' or 'B', across every truth-table bucket.
        let tuples = [
            (0, 0, 0, 0, 0), // empty → C
            (0, 1, 0, 0, 0), // below majority → C
            (0, 3, 0, 0, 0), // runs<4 && useful==0 → C (not yet churn-D)
            (0, 4, 0, 0, 0), // churn → D
            (0, 4, 1, 0, 0), // just under majority → C
            (0, 4, 2, 0, 0), // exact majority, clean → B
            (0, 5, 3, 0, 0), // above majority, clean → B
            (0, 5, 3, 0, 1), // above majority but errors → C
            (0, 6, 1, 0, 0), // idle-but-clean → C
            (1, 0, 0, 0, 0), // shipped → A
            (1, 3, 0, 0, 3), // shipped despite errors → A
            (2, 4, 8, 0, 0), // shipped and useful → A
            (1, 2, 2, 1, 0), // incident outranks shipping → D
            (0, 0, 0, 2, 0), // incident only → D
        ];
        for (shipped, runs, useful, incidents, errors) in tuples {
            let grade = CycleScore::grade_of(shipped, runs, useful, incidents, errors);
            assert_eq!(
                can_finalize_cycle(shipped, runs, useful, incidents, errors),
                matches!(grade.as_str(), "A" | "B"),
                "gate/grade disagree at shipped={shipped} runs={runs} useful={useful} \
                 incidents={incidents} errors={errors} (grade {grade})"
            );
        }
    }
}
