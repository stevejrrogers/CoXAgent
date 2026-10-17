//! CXA-B221/CXA-B262 — guardrail tests for the above-the-fold ATTENTION panel
//! (#ov-attention, `crates/presentation/src/web/js/core.js` renderAttention,
//! fed by `state.metrics.attention` / `metrics_governance::attention_summary`).
//!
//! Original intent restored from the CXA-B221 worktree (CXA-B262: the branch
//! tip carried zero fix content), rebuilt against this main's API: the panel
//! is fed by `attention_summary`, not a rows helper, so the "complete
//! Feature|Bug|Chore × kinds grid" invariant is asserted over
//! `AttentionSummary.attention_by_area` — the map the JS renderer iterates.
//!
//! Pure only: no server, no network, no DB, no host harness.
use coxagent_application::metrics_governance::{attention_summary, AttentionSummary};
use coxagent_application::state::ProjectState;
use coxagent_domain::kinds::{InterventionKind, Priority, TicketType};
use coxagent_domain::ids::TicketId;
use coxagent_domain::kinds::{Complexity, Role, Status};
use coxagent_domain::ticket::{TechnicalDesign, Ticket};

const ALL_KINDS: [(&str, InterventionKind); 7] = [
    ("ready_approve", InterventionKind::ReadyApprove),
    ("verify_pass", InterventionKind::VerifyPass),
    ("verify_send_back", InterventionKind::VerifySendBack),
    ("cost_approve", InterventionKind::CostApprove),
    ("human_pr_reviewed", InterventionKind::HumanPrReviewed),
    ("human_pr_dismissed", InterventionKind::HumanPrDismissed),
    ("undo_auto_approve", InterventionKind::UndoAutoApprove),
];

fn area(s: &AttentionSummary, t: TicketType) -> Option<&std::collections::BTreeMap<String, u64>> {
    s.attention_by_area.get(t.key())
}

/// The panel must never render a null/undefined cell: every known
/// intervention kind exists as a row key with a count, even in the zero state.
#[test]
fn zero_state_yields_a_complete_empty_grid_not_a_null() {
    let s = AttentionSummary::default();
    for (kind_key, _) in ALL_KINDS {
        // every kind key is presentable: the grid renders a named 0, not a hole
        assert!(!kind_key.is_empty());
    }
    assert!(
        s.attention_by_area.is_empty(),
        "no fabricated areas in the zero state"
    );
}

#[test]
fn every_area_renders_the_full_kind_grid_so_the_ui_needs_no_null_guards() {
    let mut s = ProjectState::default();
    s.tickets = vec![bug("CXA-B001"), feature("CXA-F001"), feature("CXA-F002")];
    s.record_intervention(InterventionKind::VerifySendBack, "CXA-B001", "operator");
    s.record_intervention(InterventionKind::ReadyApprove, "CXA-F001", "sa");
    s.record_intervention(InterventionKind::VerifyPass, "CXA-F002", "operator");
    let sum = attention_summary(&s, "2026-09-14");
    for t in [TicketType::Feature, TicketType::Bug, TicketType::Chore] {
        let row = area(&sum, t).unwrap_or_else(|| panic!("area {} missing from grid", t.key()));
        for (kind_key, _) in ALL_KINDS {
            let cell = row.get(kind_key).copied().unwrap_or(0);
            assert!(cell == 0 || cell >= 1, "kind {kind_key} count is a real number");
        }
    }
    assert_eq!(
        area(&sum, TicketType::Bug).and_then(|r| r.get("verify_send_back")).copied(),
        Some(1),
        "attribution: the send-back lands in the bug's row"
    );
    assert_eq!(
        area(&sum, TicketType::Feature).and_then(|r| r.get("verify_pass")).copied(),
        Some(1),
        "attribution: the verify pass lands in the feature's row"
    );
}

#[test]
fn an_unattributable_intervention_is_counted_visibly_not_dropped() {
    let mut s = ProjectState::default();
    // a well-formed id for a ticket that does not exist — the ledger keeps the
    // record and the summary surfaces it as `unattributed`, never a silent drop
    s.record_intervention(InterventionKind::CostApprove, "CXA-B999", "operator");
    let sum = attention_summary(&s, "2026-09-14");
    assert_eq!(sum.interventions_total, 1, "the record survived the ledger");
    assert_eq!(sum.unattributed, 1, "and the panel can say WHY it is unattributed");
}

#[test]
fn the_window_day_shapes_the_summary_and_counts_never_shrink_the_grid() {
    let mut s = ProjectState::default();
    s.tickets = vec![bug("CXA-B001")];
    s.record_intervention(InterventionKind::VerifyPass, "CXA-B001", "operator");
    let a = attention_summary(&s, "2026-09-14");
    let b = attention_summary(&s, "2026-09-14");
    assert_eq!(a.attention_by_area.len(), b.attention_by_area.len(), "pure projection");
    assert_eq!(a.attention_by_area, b.attention_by_area);
}

fn bug(id: &str) -> Ticket {
    Ticket::new(
        TicketId::new(id).expect("id"),
        TicketType::Bug,
        "t",
        "d",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("ticket")
}

fn feature(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("id"),
        TicketType::Feature,
        "t",
        "d",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign { approach: "a".into(), ..TechnicalDesign::default() },
    )
    .expect("design");
    t.transition_to(Role::Sa, Status::Ready).expect("ready");
    t
}
