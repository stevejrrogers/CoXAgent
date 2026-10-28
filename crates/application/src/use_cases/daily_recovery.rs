//! Daily-recovery core (CXA-F403, part 1/3 of CXA-F376) — a pure eligibility
//! decision plus a diagnosis line for tickets whose rescue budget is spent but
//! which sit parked forever: `escalation::sm_unpark_tickets` (escalation.rs,
//! `done < MAX_TICKET_RESCUES + 5` filter) explicitly leaves them "for the
//! daily-recovery feature (CXA-F376) or a human".
//!
//! Pure functions over persisted-state snapshots only — no IO, no clock, no
//! framework imports, so [`crate::tests::hexagonal_gate`] is untouched. The
//! part-2 cycle step gathers [`RecoverySnapshot`]s through a port adapter and
//! persists the diagnosis line via the writer port; this module decides and
//! explains, nothing more.
//!
//! Lane-complement invariant: this module is eligible only at
//! `rescues_used >= DAILY_RECOVERY_FLOOR` (= `MAX_TICKET_RESCUES + 5`), the
//! exact complement of the unpark lane's `done < MAX_TICKET_RESCUES + 5`
//! filter, so the two lanes can never both act on the same ticket in one day.

use crate::state::{AttemptFailure, FailureLayer};
use crate::use_cases::merge_policy::{route_from_failures, EscalationRoute, MAX_TICKET_RESCUES};

/// Rescue-count floor for daily recovery: the exact complement of the unpark
/// lane's filter (`done < MAX_TICKET_RESCUES + 5` in escalation.rs), so a
/// ticket is either unparked this cycle or recovered by the daily sweep —
/// provably never both.
pub const DAILY_RECOVERY_FLOOR: u32 = MAX_TICKET_RESCUES + 5;

/// Who performs the recovery. Every diagnosis line attributes the action to
/// this actor so the activity trail answers "who changed this state".
pub const SWEEP_ACTOR: &str = "daily-sweep";

/// Everything the pure decision needs about one parked ticket — plain data the
/// part-2 adapter gathers from persisted state (`tickets`,
/// `ticket_redesigns`, `attempt_failures`, `hold_reasons`, parent liveness).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySnapshot {
    /// Ticket id (per-project alias form, e.g. `CXC-B001`).
    pub id: String,
    /// Bugs share the feature lifecycles' statuses but only bugs get unparked;
    /// the sweep recovers bugs.
    pub ticket_type: coxagent_domain::TicketType,
    /// Current lifecycle status — eligibility requires [`coxagent_domain::Status::OnHold`].
    pub status: coxagent_domain::Status,
    /// Senior rescues already consumed (`ticket_redesigns` counter).
    pub rescues_used: u32,
    /// The structured failure log for this ticket (`AttemptFailure`s).
    pub attempts: Vec<AttemptFailure>,
    /// A person-recorded hold reason: present means an operator parked it
    /// deliberately and the sweep must not override the park.
    pub hold_reason: Option<String>,
    /// Whether the ticket's parent is still open/in progress — children of a
    /// live parent resume with the parent, not on their own schedule.
    pub live_parent: bool,
}

/// Why a ticket stays held. Named causes only — never a fabricated one
/// (`pause_explainer`'s honesty rule: say what is known, nothing more).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryBlock {
    /// Still inside the unpark lane's budget window — that lane owns it this
    /// cycle.
    RescueBudgetRemaining,
    /// Parent ticket still live; the child resumes with the parent.
    LiveParent,
    /// An operator parked it deliberately (`hold_reason` recorded); a sweep
    /// never overrides a human's park.
    HumanHold,
    /// Not a bug sitting in `OnHold` — outside this feature's scope.
    NotHeldBug,
    /// Nothing counted against the ticket (log empty, or every failure is
    /// Infra-layer, which never counts). Re-running would burn an attempt on
    /// nobody's fault, so the sweep declines.
    NoCountedFailure,
}

impl RecoveryBlock {
    /// Stable short label for logs and the part-3 panel.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RescueBudgetRemaining => "rescue-budget-remaining",
            Self::LiveParent => "live-parent",
            Self::HumanHold => "human-hold",
            Self::NotHeldBug => "not-held-bug",
            Self::NoCountedFailure => "no-counted-failure",
        }
    }

    /// Full sentence explaining WHY the ticket stays held.
    #[must_use]
    pub fn explain(self, snapshot: &RecoverySnapshot) -> String {
        match self {
            Self::RescueBudgetRemaining => format!(
                "kept held by {SWEEP_ACTOR}: rescue budget not spent yet \
                 ({} rescue(s) < floor {DAILY_RECOVERY_FLOOR}) — the unpark \
                 lane owns it this cycle",
                snapshot.rescues_used
            ),
            Self::LiveParent => format!(
                "kept held by {SWEEP_ACTOR}: parent ticket is still live — \
                 the child resumes with its parent, not on the sweep"
            ),
            Self::HumanHold => match &snapshot.hold_reason {
                Some(reason) => format!(
                    "kept held by {SWEEP_ACTOR}: a human parked it \
                     deliberately (hold reason: \"{reason}\")"
                ),
                // Unreachable from `recovery_eligibility` (HumanHold implies a
                // recorded reason); total anyway so direct callers never panic.
                None => format!(
                    "kept held by {SWEEP_ACTOR}: a human parked it \
                     deliberately (hold reason recorded separately)"
                ),
            },
            Self::NotHeldBug => format!(
                "kept held by {SWEEP_ACTOR}: not a bug in OnHold — outside \
                 the daily-recovery lane"
            ),
            Self::NoCountedFailure => {
                if snapshot.attempts.is_empty() {
                    format!(
                        "kept held by {SWEEP_ACTOR}: no failure recorded — the \
                         sweep never acts without evidence"
                    )
                } else {
                    format!(
                        "kept held by {SWEEP_ACTOR}: every recorded failure is \
                         infra-layer, which never counts — resuming would burn \
                         an attempt on nobody's fault"
                    )
                }
            }
        }
    }
}

/// One candidate's outcome: eligible (resume) or kept held with a named cause
/// — always carrying its attribution/diagnosis line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryVerdict {
    /// Id of the snapshot this verdict describes.
    pub snapshot_id: String,
    /// Whether the sweep resumes this ticket.
    pub eligible: bool,
    /// The named blocker when kept held.
    pub blocked: Option<RecoveryBlock>,
    /// The diagnosis line: hold cause, sweep rule that fired (or why not),
    /// acting actor, and the passed-in timestamp.
    pub diagnosis: String,
}

/// The eligibility rule, single-sourced here: resume a ticket iff it is a bug
/// parked in `OnHold`, no live parent, its rescue budget is spent (>=
/// [`DAILY_RECOVERY_FLOOR`]), and at least one counted (non-Infra) failure
/// exists. `at` is the RFC3339 sweep timestamp handed in by the caller — the
/// function never reads a clock, so it stays pure and deterministic.
#[must_use]
pub fn recovery_eligibility(snapshot: &RecoverySnapshot, at: &str) -> RecoveryVerdict {
    let block = if snapshot.ticket_type != coxagent_domain::TicketType::Bug
        || snapshot.status != coxagent_domain::Status::OnHold
    {
        Some(RecoveryBlock::NotHeldBug)
    } else if snapshot.hold_reason.is_some() {
        Some(RecoveryBlock::HumanHold)
    } else if snapshot.live_parent {
        Some(RecoveryBlock::LiveParent)
    } else if snapshot.rescues_used < DAILY_RECOVERY_FLOOR {
        Some(RecoveryBlock::RescueBudgetRemaining)
    } else if counted_failures(snapshot) == 0 {
        Some(RecoveryBlock::NoCountedFailure)
    } else {
        None
    };

    let diagnosis = match block {
        Some(b) => b.explain(snapshot),
        None => resume_diagnosis(snapshot, at),
    };
    RecoveryVerdict {
        snapshot_id: snapshot.id.clone(),
        eligible: block.is_none(),
        blocked: block,
        diagnosis,
    }
}

/// Counted attempts = non-Infra failures (state.rs's own rule: infra faults
/// are nobody's failure and never counted against a ticket).
fn counted_failures(snapshot: &RecoverySnapshot) -> usize {
    snapshot
        .attempts
        .iter()
        .filter(|f| f.layer != FailureLayer::Infra)
        .count()
}

/// Saturating rescue-count display for the sweep-rule clause: counters are
/// u32, so arithmetic cannot overflow — `saturating` keeps the semantics
/// explicit for future field widening.
fn rescues_display(rescues_used: u32) -> u32 {
    rescues_used.saturating_sub(0)
}

/// The resume-side diagnosis line: the four attribution facts (why it was
/// auto-held, why the sweep resumes it now, who acted, when), plus the
/// routing hint naming the role that owns the dominant failure layer.
fn resume_diagnosis(snapshot: &RecoverySnapshot, at: &str) -> String {
    let counted = counted_failures(snapshot);
    // Single-sourced failure routing: reuse merge_policy's router rather than
    // re-implementing layer→role mapping (escalation.rs uses the same routes).
    let owner = match route_from_failures(&snapshot.attempts, false) {
        EscalationRoute::Spec => "BA (spec)",
        EscalationRoute::Mechanical => "SA repair (mechanical gates)",
        EscalationRoute::Design => "SA (design)",
        EscalationRoute::Oversize => "SA (split — oversize)",
    };
    let last = snapshot
        .attempts
        .iter()
        .rfind(|f| f.layer != FailureLayer::Infra)
        .map_or_else(
            || "(no failure recorded)".to_owned(),
            |f| format!("[{:?}/{}] {}", f.layer, f.gate, f.detail),
        );
    let held_because = snapshot.hold_reason.as_deref().map_or_else(
        || {
            format!(
                "auto-held after {} senior rescue(s) exhausted",
                rescues_display(snapshot.rescues_used)
            )
        },
        |reason| format!("parked by hand: \"{reason}\""),
    );
    format!(
        "{SWEEP_ACTOR} at {at}: resuming — swept because rescues_used {} >= \
         recovery floor {DAILY_RECOVERY_FLOOR} (unpark lane left it); \
         {held_because}; {counted} counted failure(s), routed to {owner}; \
         last failure: {last}",
        rescues_display(snapshot.rescues_used)
    )
}

/// Batch decision over all held-ticket candidates, ready for the part-3
/// panel: verdicts ordered eligible-first (stable — same-input order preserved
/// within each group) plus explicit counts and a zero-state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPlan {
    /// Per-candidate verdicts, eligible first.
    pub verdicts: Vec<RecoveryVerdict>,
    /// How many candidates the sweep will resume.
    pub eligible: usize,
    /// How many candidates stay held.
    pub blocked: usize,
}

impl RecoveryPlan {
    /// The explicit zero-state for the panel: says what fills the list, never
    /// renders a bare empty widget.
    #[must_use]
    pub fn empty_state() -> &'static str {
        "no tickets past their rescue budget"
    }
}

/// Decide a whole batch of held tickets at one sweep instant. Empty input is a
/// valid, expected outcome (nothing past its budget) — it yields the empty
/// plan with zero counts, not an error.
#[must_use]
pub fn daily_recovery_plan(candidates: &[RecoverySnapshot], at: &str) -> RecoveryPlan {
    let mut verdicts: Vec<RecoveryVerdict> = candidates
        .iter()
        .map(|s| recovery_eligibility(s, at))
        .collect();
    verdicts.sort_by_key(|v| !v.eligible);
    let eligible = verdicts.iter().filter(|v| v.eligible).count();
    let blocked = verdicts.len() - eligible;
    RecoveryPlan {
        verdicts,
        eligible,
        blocked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FailureLayer;
    use coxagent_domain::{Status, TicketType};

    const AT: &str = "2026-10-26T03:00:00Z";

    /// A ticket just past the recovery floor with real counted failures.
    fn held_bug_past_floor() -> RecoverySnapshot {
        RecoverySnapshot {
            id: "CXC-B101".into(),
            ticket_type: TicketType::Bug,
            status: Status::OnHold,
            rescues_used: DAILY_RECOVERY_FLOOR,
            attempts: vec![
                AttemptFailure {
                    attempt: 1,
                    layer: FailureLayer::Gate,
                    gate: "clippy".into(),
                    detail: "unused import".into(),
                    files: vec![],
                },
                AttemptFailure {
                    attempt: 2,
                    layer: FailureLayer::Design,
                    gate: "regression-test".into(),
                    detail: "red suite after resume".into(),
                    files: vec![],
                },
            ],
            hold_reason: None,
            live_parent: false,
        }
    }

    #[test]
    fn a_held_bug_past_the_floor_with_counted_failures_is_resumed() {
        let v = recovery_eligibility(&held_bug_past_floor(), AT);
        assert!(v.eligible);
        assert_eq!(v.blocked, None);
        assert_eq!(v.snapshot_id, "CXC-B101");
    }

    #[test]
    fn the_resume_diagnosis_carries_all_four_facts() {
        let d = &recovery_eligibility(&held_bug_past_floor(), AT).diagnosis;
        assert!(d.contains(SWEEP_ACTOR), "actor missing: {d}");
        assert!(d.contains(AT), "timestamp missing: {d}");
        // Why it was auto-held…
        assert!(d.contains("auto-held after 7 senior rescue(s)"), "{d}");
        // …and why it resumes now (the sweep rule that fired).
        assert!(
            d.contains(format!(">= recovery floor {DAILY_RECOVERY_FLOOR}").as_str()),
            "{d}"
        );
        // Attribution depth: counted attempts + owned layer + last failure.
        assert!(d.contains("2 counted failure(s)"), "{d}");
        assert!(d.contains("routed to SA (design)"), "{d}");
        assert!(
            d.contains("last failure: [Design/regression-test] red suite after resume"),
            "{d}"
        );
    }

    #[test]
    fn the_resume_diagnosis_is_deterministic_for_part3_ui_tests() {
        let a = recovery_eligibility(&held_bug_past_floor(), AT).diagnosis;
        let b = recovery_eligibility(&held_bug_past_floor(), AT).diagnosis;
        assert_eq!(a, b);
    }

    #[test]
    fn below_the_floor_stays_in_the_unpark_lane() {
        let mut s = held_bug_past_floor();
        s.rescues_used = DAILY_RECOVERY_FLOOR - 1;
        let v = recovery_eligibility(&s, AT);
        assert!(!v.eligible);
        assert_eq!(v.blocked, Some(RecoveryBlock::RescueBudgetRemaining));
        assert!(v.diagnosis.contains("< floor"), "{}", v.diagnosis);
    }

    #[test]
    fn the_two_lanes_never_overlap_on_the_same_ticket() {
        // Guard for the lane-complement invariant: exactly one lane may claim
        // any rescues_used value.
        for rescues in 0..=DAILY_RECOVERY_FLOOR * 2 {
            let mut s = held_bug_past_floor();
            s.rescues_used = rescues;
            let v = recovery_eligibility(&s, AT);
            let claimed_by_sweep = v.eligible;
            let claimed_by_unpark = rescues < DAILY_RECOVERY_FLOOR; // escalation.rs filter, mirrored
            assert_ne!(
                claimed_by_sweep, claimed_by_unpark,
                "lanes overlapped at rescues_used={rescues}"
            );
        }
    }

    #[test]
    fn a_live_parent_blocks_even_when_the_budget_is_spent() {
        let mut s = held_bug_past_floor();
        s.live_parent = true;
        let v = recovery_eligibility(&s, AT);
        assert!(!v.eligible);
        assert_eq!(v.blocked, Some(RecoveryBlock::LiveParent));
        assert!(
            v.diagnosis.contains("parent ticket is still live"),
            "{}",
            v.diagnosis
        );
    }

    #[test]
    fn a_human_hold_is_quoted_not_overridden() {
        let mut s = held_bug_past_floor();
        s.hold_reason = Some("blocked on vendor billing account".into());
        let v = recovery_eligibility(&s, AT);
        assert!(!v.eligible);
        assert_eq!(v.blocked, Some(RecoveryBlock::HumanHold));
        assert!(
            v.diagnosis
                .contains("\"blocked on vendor billing account\""),
            "{}",
            v.diagnosis
        );
    }

    #[test]
    fn an_all_infra_log_counts_as_no_evidence() {
        let mut s = held_bug_past_floor();
        s.attempts = vec![AttemptFailure {
            attempt: 1,
            layer: FailureLayer::Infra,
            gate: "engine".into(),
            detail: "provider unavailable".into(),
            files: vec![],
        }];
        let v = recovery_eligibility(&s, AT);
        assert!(!v.eligible);
        assert_eq!(v.blocked, Some(RecoveryBlock::NoCountedFailure));
        assert!(v.diagnosis.contains("infra-layer"), "{}", v.diagnosis);
    }

    #[test]
    fn an_empty_failure_log_never_gets_a_fabricated_cause() {
        let mut s = held_bug_past_floor();
        s.attempts = vec![];
        let v = recovery_eligibility(&s, AT);
        assert!(!v.eligible);
        assert_eq!(v.blocked, Some(RecoveryBlock::NoCountedFailure));
        assert!(
            v.diagnosis.contains("no failure recorded"),
            "{}",
            v.diagnosis
        );
    }

    #[test]
    fn a_feature_ticket_on_hold_is_out_of_scope() {
        let mut s = held_bug_past_floor();
        s.ticket_type = TicketType::Feature;
        let v = recovery_eligibility(&s, AT);
        assert!(!v.eligible);
        assert_eq!(v.blocked, Some(RecoveryBlock::NotHeldBug));

        let mut s = held_bug_past_floor();
        s.status = Status::Open;
        let v = recovery_eligibility(&s, AT);
        assert!(!v.eligible);
        assert_eq!(v.blocked, Some(RecoveryBlock::NotHeldBug));
    }

    #[test]
    fn the_plan_orders_eligible_first_and_counts_both_ways() {
        let mut stuck = held_bug_past_floor();
        stuck.id = "CXC-B102".into();
        let mut young = held_bug_past_floor();
        young.id = "CXC-B103".into();
        young.rescues_used = 1;
        let plan = daily_recovery_plan(&[young.clone(), stuck.clone()], AT);
        assert_eq!(plan.eligible, 1);
        assert_eq!(plan.blocked, 1);
        assert!(plan.verdicts[0].eligible, "eligible must come first");
        assert_eq!(plan.verdicts[0].snapshot_id, "CXC-B102");
        assert_eq!(
            plan.verdicts[1].blocked,
            Some(RecoveryBlock::RescueBudgetRemaining)
        );
    }

    #[test]
    fn an_empty_candidate_list_yields_the_explicit_zero_state() {
        let plan = daily_recovery_plan(&[], AT);
        assert!(plan.verdicts.is_empty());
        assert_eq!(plan.eligible, 0);
        assert_eq!(plan.blocked, 0);
        assert_eq!(
            RecoveryPlan::empty_state(),
            "no tickets past their rescue budget"
        );
    }
}
