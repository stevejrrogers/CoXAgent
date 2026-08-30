//! TDD tests for CXA-F236 — stale human-gate approval escalation across
//! fallback approvers over SLA windows (the gate-hold half; the question-SLA
//! half lives in `escalation_tdd_tests.rs`).
//!
//! AC → test map. Every fixture is built through the real state/domain types
//! (an in-memory store double and a canned engine — no server, no harness, no
//! network port; the `run_test_tdd_tests.rs` pattern):
//! - AC1 (core/pure) — [`t1_plan_flags_only_holds_past_their_sla_window`]:
//!   which waiting-human items exceed the configured SLA window, plus ordered
//!   eligible fallback approvers derived strictly from AuthRole permissions.
//! - AC2 (gate invariants preserved) —
//!   [`ac2_escalation_moves_no_ticket_and_posts_notifications_only`],
//!   [`t4_final_tier_posts_the_sm_impediment_action_then_stops_laddering`],
//!   [`ac2_a_decided_gate_resets_the_ladder`].
//! - AC3 (config-driven SLA + documented defaults) —
//!   [`t5_documented_defaults_disable_the_escalation_entirely`],
//!   [`t2_ladder_advances_once_per_crossed_boundary_and_is_idempotent`].
//!
//! The SA design contract this encodes: `human.gate_sla_minutes` (0 = never
//! escalate) + `human.gate_escalation_tiers` (ordered cumulative windows →
//! widened role sets, filtered by the AuthRole gate predicates); state gains
//! `gate_holds {entered_at_unix_s, escalated_to_tier}` (missing = not
//! escalated; entry time from the activity journal's status entry where
//! available, else first observation — never a fabricated age).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::gate_escalation::{
    ordered_fallback_approvers, plan_gate_escalations, waiting_gate_items, GateKind,
};
use super::RunCycleUseCase;
use crate::auth::AuthRole;
use crate::config::{Config, GateEscalationTier};
use crate::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus, StateStorePort,
};
use crate::state::{ActivityEntry, GateHold, ProjectState, AGENTS_CHANNEL};
use crate::PortError;
use coxagent_domain::transitions::can_transition;
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};

#[derive(Default)]
struct MemStore {
    state: Mutex<ProjectState>,
}

#[async_trait::async_trait]
impl StateStorePort for MemStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().expect("lock").clone())
    }
    async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
        s.validate().map_err(PortError::Corrupt)?;
        *self.state.lock().expect("lock") = s.clone();
        Ok(())
    }
}

/// The escalation phase never calls the engine; the double only satisfies the
/// use case's constructor.
struct SilentEngine;

#[async_trait::async_trait]
impl AgentEnginePort for SilentEngine {
    fn id(&self) -> &'static str {
        "silent"
    }
    async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
        Ok(AgentOutcome {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            usage: None,
            trace: String::new(),
            session_id: None,
            sandbox: SandboxStatus::default(),
            engine: String::new(),
        })
    }
}

/// Unix seconds `minutes` ago — the clock every age in this feature is
/// measured on (`now_unix_s` in the module under test).
fn minutes_ago_s(minutes: i64) -> i64 {
    crate::use_cases::cycle::gate_escalation::now_unix_s() - minutes * 60
}

/// An RFC3339 timestamp `minutes` in the past, for activity-journal seeding.
fn minutes_ago_rfc(minutes: i64) -> String {
    (time::OffsetDateTime::now_utc() - time::Duration::minutes(minutes))
        .format(&time::format_description::well_known::Rfc3339)
        .expect("rfc3339")
}

/// A ready-gate item's shape: designed, waiting in Pending for a person.
fn pending_designed_feature(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid id"),
        TicketType::Feature,
        format!("feature {id}"),
        "fixture",
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("valid ticket");
    t.set_technical_design(
        Role::Sa,
        coxagent_domain::TechnicalDesign {
            approach: "fixture design".to_owned(),
            ..coxagent_domain::TechnicalDesign::default()
        },
    )
    .expect("SA attaches the design");
    t
}

/// A verify-gate item's shape: fixed, evidence attached, awaiting a verdict.
fn fixed_bug(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid id"),
        TicketType::Bug,
        format!("bug {id}"),
        "fixture",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    t.claim(Role::DevBug, "dev@host", "2026-08-30T00:00:00Z")
        .expect("unclaimed bug is claimable");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("dev may mark a claimed bug fixed");
    t
}

fn human_config(sla: u64, tiers: Vec<GateEscalationTier>) -> crate::config::HumanConfig {
    crate::config::HumanConfig {
        gate_ready: true,
        gate_verify: true,
        gate_sla_minutes: sla,
        gate_escalation_tiers: tiers,
        ..crate::config::HumanConfig::default()
    }
}

fn ladder() -> Vec<GateEscalationTier> {
    vec![
        GateEscalationTier {
            after_minutes: 30,
            roles: vec!["sm".to_owned()],
        },
        GateEscalationTier {
            after_minutes: 60,
            roles: vec!["techlead".to_owned()],
        },
        GateEscalationTier {
            after_minutes: 90,
            roles: vec!["admin".to_owned()],
        },
    ]
}

fn hold_at(minutes_ago: i64, tier: u32) -> GateHold {
    GateHold {
        entered_at_unix_s: minutes_ago_s(minutes_ago),
        escalated_to_tier: tier,
        at_ready_gate: true,
    }
}

fn verify_hold_at(minutes_ago: i64, tier: u32) -> GateHold {
    GateHold {
        at_ready_gate: false,
        ..hold_at(minutes_ago, tier)
    }
}

async fn statuses(store: &MemStore) -> Vec<(String, Status)> {
    store
        .load()
        .await
        .expect("load")
        .tickets
        .iter()
        .map(|t| (t.id().to_string(), t.status()))
        .collect()
}

/// t1 (AC1, pure) — the plan flags exactly the gate holds past the configured
/// SLA window, with the right gate kind, and the waiting set mirrors the
/// hybrid inbox (a Pending ticket without a design waits at no gate).
#[test]
fn t1_plan_flags_only_holds_past_their_sla_window() {
    let human = human_config(30, ladder());
    let mut state = ProjectState::default();
    // Ready-gate item, persisted hold 120m old → stale.
    state.tickets.push(pending_designed_feature("CXC-F236A"));
    state
        .gate_holds
        .insert("CXC-F236A".to_owned(), hold_at(120, 0));
    // Ready-gate item inside its window → not stale.
    state.tickets.push(pending_designed_feature("CXC-F236B"));
    state
        .gate_holds
        .insert("CXC-F236B".to_owned(), hold_at(5, 0));
    // Ready-gate item with no persisted hold: the ready gate records no
    // wait-start of its own, so the clock derives from the activity journal's
    // status entry (design landed 120m ago) → stale.
    state.tickets.push(pending_designed_feature("CXC-F236D"));
    state.activity.push(ActivityEntry {
        at: minutes_ago_rfc(120),
        agent: "SA".to_owned(),
        action: "designed (technical)".to_owned(),
        ticket: Some("CXC-F236D".to_owned()),
    });
    // Verify-gate item, entry derived from the activity journal's status
    // entry (no persisted hold) 200m ago → stale.
    state.tickets.push(fixed_bug("CXC-F236C"));
    state.add_evidence("CXC-F236C", "api", "GET /health", "200 ok");
    state.activity.push(ActivityEntry {
        at: minutes_ago_rfc(200),
        agent: "DEV-BUG".to_owned(),
        action: "fixed bug".to_owned(),
        ticket: Some("CXC-F236C".to_owned()),
    });
    // A designed ticket while the gate is OFF waits at no gate at all.
    let mut off = human_config(30, ladder());
    off.gate_verify = false;
    assert!(
        !waiting_gate_items(&state, &off)
            .iter()
            .any(|(_, k)| *k == GateKind::Verify),
        "gate_verify off → no verify-gate items"
    );

    let plan = plan_gate_escalations(&state, &human, minutes_ago_s(0));

    let flagged: Vec<(String, GateKind)> = plan
        .escalations
        .iter()
        .map(|e| (e.ticket.clone(), e.kind))
        .collect();
    assert_eq!(
        flagged,
        vec![
            ("CXC-F236A".to_owned(), GateKind::Ready),
            ("CXC-F236D".to_owned(), GateKind::Ready),
            ("CXC-F236C".to_owned(), GateKind::Verify),
        ],
        "only holds past the SLA, never items inside their window"
    );
    // AC1's second half: ordered eligible fallback approvers derived strictly
    // from AuthRole permissions — the verify hold's final audience.
    let verify_esc = plan
        .escalations
        .iter()
        .find(|e| e.kind == GateKind::Verify)
        .expect("the verify hold escalated");
    let final_audience = &verify_esc.advances.last().expect("advanced").audience;
    assert!(
        final_audience.contains(&AuthRole::Qa),
        "the verify gate's owner leads its fallback chain"
    );
    for role in final_audience {
        assert!(
            GateKind::Verify.permits(*role),
            "{role:?} cannot take the verify decision — never escalated-to"
        );
    }
}

/// t2 (AC3, pure) — a hold that leapfrogs several windows in one pass crosses
/// each tier boundary exactly once (three windows elapsed → three rungs,
/// final tier reached), and a second pass over the advanced state plans
/// nothing: idempotent between passes, no tier ever repeats.
#[test]
fn t2_ladder_advances_once_per_crossed_boundary_and_is_idempotent() {
    let human = human_config(30, ladder());
    let mut state = ProjectState::default();
    state.tickets.push(pending_designed_feature("CXC-F236A"));
    state
        .gate_holds
        .insert("CXC-F236A".to_owned(), hold_at(100, 0));
    let now = minutes_ago_s(0);

    let plan = plan_gate_escalations(&state, &human, now);
    let esc = &plan.escalations[0];
    let tiers_crossed: Vec<u32> = esc.advances.iter().map(|a| a.tier).collect();
    assert_eq!(tiers_crossed, vec![1, 2, 3], "one rung per boundary crossing");
    assert!(
        esc.advances.last().expect("final rung").final_tier,
        "the last configured tier is the final-tier path"
    );
    // Apply exactly what the plan records, then re-plan: nothing left.
    state
        .gate_holds
        .insert("CXC-F236A".to_owned(), hold_at(100, 3));
    let second = plan_gate_escalations(&state, &human, now);
    assert!(
        second.escalations.is_empty(),
        "a pass may never re-advance an already-reached tier"
    );
}

/// Regression (self-review of CXA-F236): a hold carried across a gate CHANGE
/// is not the same wait — a ticket that escalated at the ready gate, was
/// approved, and later reached the verify gate starts its verify wait FRESH
/// (tier 0; clock from first observation when the journal has nothing), never
/// pre-aged or pre-escalated by the previous gate's hold.
#[test]
fn a_hold_from_the_other_gate_does_not_pre_age_the_new_wait() {
    let human = human_config(30, ladder());
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug("CXC-F236A"));
    state.add_evidence("CXC-F236A", "api", "GET /health", "200 ok");
    // Left over from the ticket's READY-gate wait: escalated to tier 2 with a
    // 400m clock; the journal carries no verify-side status entry.
    state
        .gate_holds
        .insert("CXC-F236A".to_owned(), hold_at(400, 2));
    // Positive control: a hold of the SAME kind is honored — CXC-F236B's
    // verify wait really is 400m old and past the first tier.
    state.tickets.push(fixed_bug("CXC-F236B"));
    state.add_evidence("CXC-F236B", "api", "GET /metrics", "200 ok");
    state
        .gate_holds
        .insert("CXC-F236B".to_owned(), verify_hold_at(400, 1));

    let plan = plan_gate_escalations(&state, &human, minutes_ago_s(0));

    assert!(
        !plan.escalations.iter().any(|e| e.ticket == "CXC-F236A"),
        "a new wait must not inherit the previous gate's escalation"
    );
    let hold = plan
        .upsert_holds
        .iter()
        .find(|(id, _)| id == "CXC-F236A")
        .map(|(_, h)| *h)
        .expect("the fresh wait is recorded");
    assert!(!hold.at_ready_gate, "the hold now belongs to the verify gate");
    assert_eq!(hold.escalated_to_tier, 0, "tier reset for the new gate");
    assert!(
        minutes_ago_s(0) - hold.entered_at_unix_s <= 1,
        "clock restarted at first observation, not inherited"
    );
    // The same-kind control kept its clock and escalated.
    let control = plan
        .escalations
        .iter()
        .find(|e| e.ticket == "CXC-F236B")
        .expect("a same-kind hold is honored, not discarded");
    assert_eq!(control.age_minutes, 400);
}

/// t3 (AC1, pure) — widening only ever ADDS eligible approvers: a role the
/// AuthRole predicate forbids is dropped (policy guardrails), roles already
/// enabled are never disabled, and each level's audience is a superset of the
/// one before it.
#[test]
fn t3_widening_enables_formerly_disabled_roles_and_disables_none() {
    let tiers = vec![
        GateEscalationTier {
            after_minutes: 30,
            roles: vec!["sm".to_owned(), "techlead".to_owned()],
        },
        GateEscalationTier {
            after_minutes: 60,
            roles: vec!["po".to_owned(), "qa".to_owned()],
        },
    ];
    let base = ordered_fallback_approvers(GateKind::Ready, &tiers, 0);
    assert_eq!(base, vec![AuthRole::Ba, AuthRole::Po], "the gate owners lead");

    let after_tier_1 = ordered_fallback_approvers(GateKind::Ready, &tiers, 1);
    assert!(
        !after_tier_1.contains(&AuthRole::Sm),
        "SM cannot approve_ready — a guardrailed role is never escalated-to"
    );
    assert!(
        after_tier_1.contains(&AuthRole::TechLead),
        "the formerly-disabled permitted role is enabled by the tier"
    );
    for r in &base {
        assert!(after_tier_1.contains(r), "widening disabled {r:?}");
    }

    let after_tier_2 = ordered_fallback_approvers(GateKind::Ready, &tiers, 2);
    for r in &after_tier_1 {
        assert!(after_tier_2.contains(r), "tier 2 disabled {r:?}");
    }
    assert!(
        !after_tier_2.contains(&AuthRole::Qa),
        "QA cannot approve_ready — dropped on the ready gate too"
    );

    // Same ladder, verify gate: only verify-permitted roles survive.
    let verify = ordered_fallback_approvers(GateKind::Verify, &tiers, 2);
    assert_eq!(verify.first(), Some(&AuthRole::Qa));
    for role in &verify {
        assert!(GateKind::Verify.permits(*role));
    }
}

/// t4 + AC2 (apply, through the use case and the store port) — reaching the
/// final tier posts the SM impediment action instead of laddering further,
/// escalation moves NO ticket and comments on NOTHING (the gate decisions
/// stay human per gate_promises.rs), and a second pass repeats nothing.
#[tokio::test]
async fn t4_final_tier_posts_the_sm_impediment_action_then_stops_laddering() {
    let mut state = ProjectState::default();
    state.tickets.push(pending_designed_feature("CXC-F236A"));
    state.tickets.push(fixed_bug("CXC-F236B"));
    state.add_evidence("CXC-F236B", "api", "GET /health", "200 ok");
    state
        .gate_holds
        .insert("CXC-F236A".to_owned(), hold_at(500, 0));

    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    let config = Config {
        workflow: crate::config::WorkflowConfig {
            human: human_config(30, ladder()),
            ..crate::config::WorkflowConfig::default()
        },
        ..Config::default()
    };
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(SilentEngine),
        config,
        PathBuf::from("/tmp"),
        String::new(),
    );
    let before = statuses(&store).await;
    uc.escalate_stale_gate_holds().await;

    let after = statuses(&store).await;
    assert_eq!(before, after, "escalation moved a ticket — a transition side-effect");
    let state = store.load().await.expect("load");
    assert!(
        state.comments.is_empty(),
        "escalation must not comment on tickets: {:?}",
        state.comments
    );
    // Two tiers configured → two notifications, the last one the final-tier
    // SM impediment action, not another ladder rung.
    let msgs = state.chat_in(AGENTS_CHANNEL);
    assert_eq!(msgs.len(), 3, "{msgs:?}");
    assert!(msgs[0].body.contains("escalation tier 1"), "{:?}", msgs[0].body);
    assert!(
        msgs[2].body.contains("FINAL escalation tier 3"),
        "the final tier posts the SM impediment: {:?}",
        msgs[2].body
    );
    assert_eq!(
        state.gate_holds.get("CXC-F236A").map(|h| h.escalated_to_tier),
        Some(3),
        "the reached tier is persisted"
    );

    // Second pass: at the final tier there is no further laddering and no
    // repeated noise.
    uc.escalate_stale_gate_holds().await;
    let state = store.load().await.expect("load");
    assert_eq!(
        state.chat_in(AGENTS_CHANNEL).len(),
        3,
        "the final tier must not re-ladder or repeat"
    );
}

/// AC2 — a human decision (the ticket leaving its gate) resets the ladder:
/// the hold entry is pruned, so a future wait for the same ticket starts at
/// tier 0 with a fresh clock.
#[tokio::test]
async fn ac2_a_decided_gate_resets_the_ladder() {
    let mut state = ProjectState::default();
    // Escalated, then the human approved: the ticket left the ready gate.
    let mut approved = pending_designed_feature("CXC-F236A");
    approved
        .transition_to(Role::Sa, Status::Ready)
        .expect("designed feature may be readied");
    state.tickets.push(approved);
    state
        .gate_holds
        .insert("CXC-F236A".to_owned(), hold_at(500, 3));

    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    let config = Config {
        workflow: crate::config::WorkflowConfig {
            human: human_config(30, ladder()),
            ..crate::config::WorkflowConfig::default()
        },
        ..Config::default()
    };
    RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(SilentEngine),
        config,
        PathBuf::from("/tmp"),
        String::new(),
    )
    .escalate_stale_gate_holds()
    .await;

    let state = store.load().await.expect("load");
    assert!(
        !state.gate_holds.contains_key("CXC-F236A"),
        "a decided gate must not keep its escalation tier"
    );
    assert!(state.chat_in(AGENTS_CHANNEL).is_empty());
}

/// t5 (AC3, pure + config) — the documented defaults (SLA 0, no tiers) keep
/// the exact current behaviour: an unconfigured project never escalates, no
/// matter how stale a hold is; and a pre-F236 config document loads with the
/// defaults applied.
#[test]
fn t5_documented_defaults_disable_the_escalation_entirely() {
    let human = crate::config::HumanConfig::default();
    assert_eq!(
        human.gate_sla_minutes, 0,
        "the documented default SLA is 0 = never escalate"
    );
    assert!(
        human.gate_escalation_tiers.is_empty(),
        "the documented default ladder is empty = disabled"
    );

    let mut state = ProjectState::default();
    // Gates ON but the SLA at its documented default: disabled is about the
    // SLA and empty tiers, not the gates — and still nothing escalates,
    // however stale the hold.
    let human = human_config(0, Vec::new());
    state.tickets.push(pending_designed_feature("CXC-F236A"));
    state
        .gate_holds
        .insert("CXC-F236A".to_owned(), hold_at(10_000, 0));

    let plan = plan_gate_escalations(&state, &human, minutes_ago_s(0));
    assert!(
        plan.escalations.is_empty() && plan.upsert_holds.is_empty(),
        "disabled config = no timeout, exact current behaviour"
    );

    // A pre-F236 human section loads unchanged with the defaults applied
    // (mirrors the focus_windows round-trip convention).
    let old = r#"{"gate_ready":true,"question_sla_minutes":30}"#;
    let loaded: crate::config::HumanConfig =
        serde_json::from_str(old).expect("old human config loads");
    assert_eq!(loaded.gate_sla_minutes, 0);
    assert!(loaded.gate_escalation_tiers.is_empty());
}

/// AC2 — the promised human-gate edges stay HUMAN-only from the escalation's
/// side too: the transition table grants none of them to the roles the
/// escalation and the dev agents act as, while the human path stays open (the
/// escalation can only ever route a decision to a person, never take it).
#[test]
fn ac2_the_escalation_actor_cannot_take_a_promised_gate_decision() {
    use coxagent_domain::transitions::transition_allowed;
    let edges: Vec<(Status, Status, Vec<TicketType>)> = vec![
        (
            Status::Pending,
            Status::Ready,
            vec![TicketType::Feature, TicketType::Chore],
        ),
        (
            Status::Pending,
            Status::Rejected,
            vec![TicketType::Feature, TicketType::Chore],
        ),
        (
            Status::Ready,
            Status::Rejected,
            vec![TicketType::Feature, TicketType::Chore],
        ),
        (
            Status::Ready,
            Status::Pending,
            vec![TicketType::Feature, TicketType::Chore],
        ),
        (Status::Fixed, Status::Verified, vec![TicketType::Bug]),
        (Status::Fixed, Status::Open, vec![TicketType::Bug]),
    ];
    for (from, to, types) in edges {
        assert!(
            can_transition(Role::User, from, to),
            "the human path must stay open for the escalation to route to"
        );
        for actor in [Role::Sm, Role::DevFeature, Role::DevBug] {
            assert!(
                !can_transition(actor, from, to),
                "{actor:?} must not be able to take the gate decision itself"
            );
        }
        for t in types {
            assert!(
                transition_allowed(t, from, to),
                "{t:?} lost a legal gate edge — escalation must never need a new one"
            );
        }
    }
}
