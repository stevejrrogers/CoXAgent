//! TDD tests for CXA-F403 — daily-recovery core: pure eligibility decision
//! plus a diagnosis line (part 1/3 of CXA-F376). Pure functions over real
//! application types only (`AttemptFailure`/`FailureLayer`,
//! `route_from_failures`, domain `Status`/`TicketType`) — no host, no store,
//! no process, same style as `working_hours_f234_tdd.rs`.

use coxagent_application::state::{AttemptFailure, FailureLayer};
use coxagent_application::use_cases::daily_recovery::{
    daily_recovery_plan, recovery_eligibility, RecoveryBlock, RecoverySnapshot,
    DAILY_RECOVERY_FLOOR, SWEEP_ACTOR,
};
use coxagent_domain::{Status, TicketType};

const AT: &str = "2026-10-26T03:00:00Z";

fn failure(attempt: u32, layer: FailureLayer, gate: &str, detail: &str) -> AttemptFailure {
    AttemptFailure {
        attempt,
        layer,
        gate: gate.into(),
        detail: detail.into(),
        files: vec![],
    }
}

/// Held bug past its rescue budget with two counted failures.
fn eligible_snapshot() -> RecoverySnapshot {
    RecoverySnapshot {
        id: "CXC-B201".into(),
        ticket_type: TicketType::Bug,
        status: Status::OnHold,
        rescues_used: DAILY_RECOVERY_FLOOR,
        attempts: vec![
            failure(1, FailureLayer::Gate, "clippy", "unused import"),
            failure(
                2,
                FailureLayer::Design,
                "regression-test",
                "red suite after resume",
            ),
        ],
        hold_reason: None,
        live_parent: false,
    }
}

#[test]
fn ac1_a_held_bug_at_the_floor_with_counted_gate_and_design_failures_is_resumed() {
    let v = recovery_eligibility(&eligible_snapshot(), AT);
    assert!(v.eligible, "{:?}", v.blocked);
    assert_eq!(v.blocked, None);
    // The diagnosis names the layer-owned role, the attempt count and the last
    // failure text — attribution, not just an on/off bit.
    let d = &v.diagnosis;
    assert!(d.contains("2 counted failure(s)"), "{d}");
    assert!(d.contains("routed to SA (design)"), "{d}");
    assert!(
        d.contains("last failure: [Design/regression-test] red suite after resume"),
        "{d}"
    );
}

#[test]
fn ac1_the_diagnosis_carries_all_four_required_facts() {
    let d = &recovery_eligibility(&eligible_snapshot(), AT).diagnosis;
    // why auto-held …
    assert!(d.contains("auto-held after 7 senior rescue(s)"), "{d}");
    // … why resumed now (the sweep rule that fired) …
    assert!(
        d.contains(format!(">= recovery floor {DAILY_RECOVERY_FLOOR}").as_str()),
        "{d}"
    );
    // … who acted …
    assert!(d.contains(SWEEP_ACTOR), "{d}");
    // … and when (timestamp passed in, never read from a clock).
    assert!(d.contains(AT), "{d}");
}

#[test]
fn ac2_ac8_budget_remaining_is_blocked_by_the_unpark_lane_complement() {
    let mut s = eligible_snapshot();
    s.rescues_used = DAILY_RECOVERY_FLOOR - 1;
    let v = recovery_eligibility(&s, AT);
    assert!(!v.eligible);
    assert_eq!(v.blocked, Some(RecoveryBlock::RescueBudgetRemaining));
    assert!(
        v.diagnosis.contains("unpark lane owns it this cycle"),
        "{}",
        v.diagnosis
    );

    // Lane-complement guard: for every rescue count, exactly one of the sweep
    // and `sm_unpark_tickets`' `rescues < MAX_TICKET_RESCUES + 5` filter may
    // claim the ticket.
    for rescues in 0..=DAILY_RECOVERY_FLOOR * 2 {
        s.rescues_used = rescues;
        let sweep_claims = recovery_eligibility(&s, AT).eligible;
        let unpark_claims = rescues < DAILY_RECOVERY_FLOOR;
        assert_ne!(
            sweep_claims, unpark_claims,
            "lanes overlapped at rescues_used={rescues}"
        );
    }
}

#[test]
fn ac3_a_live_parent_blocks_even_when_the_budget_is_spent() {
    let mut s = eligible_snapshot();
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
fn ac4_a_recorded_hold_reason_means_human_hold_with_the_reason_quoted() {
    let mut s = eligible_snapshot();
    s.hold_reason = Some("waiting on vendor billing fix".into());
    let v = recovery_eligibility(&s, AT);
    assert!(!v.eligible);
    assert_eq!(v.blocked, Some(RecoveryBlock::HumanHold));
    assert!(
        v.diagnosis.contains("\"waiting on vendor billing fix\""),
        "{}",
        v.diagnosis
    );
}

#[test]
fn ac5_an_all_infra_failure_log_counts_as_no_evidence() {
    let mut s = eligible_snapshot();
    s.attempts = vec![failure(
        1,
        FailureLayer::Infra,
        "engine",
        "provider unavailable",
    )];
    let v = recovery_eligibility(&s, AT);
    assert!(!v.eligible);
    assert_eq!(v.blocked, Some(RecoveryBlock::NoCountedFailure));
    assert!(v.diagnosis.contains("infra-layer"), "{}", v.diagnosis);
}

#[test]
fn ac6_an_empty_failure_log_says_so_instead_of_fabricating_a_cause() {
    let mut s = eligible_snapshot();
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
fn ac7_a_non_bug_or_not_onhold_ticket_is_out_of_scope() {
    let mut feature = eligible_snapshot();
    feature.ticket_type = TicketType::Feature;
    let v = recovery_eligibility(&feature, AT);
    assert!(!v.eligible);
    assert_eq!(v.blocked, Some(RecoveryBlock::NotHeldBug));

    let mut open = eligible_snapshot();
    open.status = Status::Open;
    let v = recovery_eligibility(&open, AT);
    assert!(!v.eligible);
    assert_eq!(v.blocked, Some(RecoveryBlock::NotHeldBug));
}

#[test]
fn ac8_the_plan_orders_eligible_first_and_counts_both_lanes() {
    let stuck = eligible_snapshot();
    let mut young = eligible_snapshot();
    young.id = "CXC-B203".into();
    young.rescues_used = 1;
    let plan = daily_recovery_plan(&[young, stuck], AT);
    assert_eq!(plan.eligible, 1);
    assert_eq!(plan.blocked, 1);
    assert!(plan.verdicts[0].eligible, "eligible first");
    assert_eq!(plan.verdicts[0].snapshot_id, "CXC-B201");
    assert_eq!(
        plan.verdicts[1].blocked,
        Some(RecoveryBlock::RescueBudgetRemaining)
    );
}

#[test]
fn ac8_empty_input_returns_an_empty_result_and_the_explicit_zero_state() {
    let plan = daily_recovery_plan(&[], AT);
    assert!(plan.verdicts.is_empty());
    assert_eq!(plan.eligible, 0);
    assert_eq!(plan.blocked, 0);
    assert_eq!(
        coxagent_application::use_cases::daily_recovery::RecoveryPlan::empty_state(),
        "no tickets past their rescue budget"
    );
}
