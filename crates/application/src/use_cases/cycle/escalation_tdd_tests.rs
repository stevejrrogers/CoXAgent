//! TDD tests for CXA-F236 — Stale human-gate approval escalation across
//! fallback approvers over SLA windows.
//!
//! AC → test map. Every fixture is built through the real state/domain types
//! (an in-memory store double and a canned engine — no server, no harness, no
//! network port; the `run_test_tdd_tests.rs` pattern):
//! - AC1 (core/pure) — WHICH waiting-human items exceed the configured SLA
//!   window: [`ac1_only_open_person_addressed_items_count_as_waiting_human_items`].
//! - AC2 (gate invariants preserved):
//!   [`ac2_escalation_issues_no_transition_side_effects_and_moves_no_ticket`],
//!   [`ac2_the_escalation_actor_cannot_take_any_promised_human_gate_decision`].
//! - AC3 (config-driven SLA, documented defaults):
//!   [`ac3_omitted_sla_is_the_documented_never_escalate_default`],
//!   [`ac3_items_within_the_configured_sla_window_stay_and_items_past_it_escalate`],
//!   [`ac3_escalation_fires_once_per_item_and_never_repeats`].
//!
//! DESIGN GAPS — previously reported, now RESOLVED by the SA's contract for
//! CXA-F236 (implemented in `gate_escalation.rs`, gate-hold tests in
//! `gate_escalation_tdd_tests.rs`): `workflow.human` gained `gate_sla_minutes`
//! (0 = never escalate) and `gate_escalation_tiers` (ordered cumulative
//! windows → widened role sets filtered by the AuthRole gate predicates);
//! approvers are `AuthRole` values (no username roster needed); state gained
//! `gate_holds {entered_at_unix_s, escalated_to_tier}` for the ready-gate
//! wait-start the state never recorded (derived from the activity journal's
//! status entry, else first observation).
//!
//! The tests below encode the question-SLA half of the escalation family:
//! the SLA-window selection semantics, the fire-once contract, and the
//! notification-only invariant the gate-hold escalation must preserve.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::RunCycleUseCase;
use crate::config::Config;
use crate::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus, StateStorePort,
};
use crate::state::{AgentQuestion, ProjectState, AGENTS_CHANNEL};
use crate::PortError;
use coxagent_domain::transitions::{can_transition, transition_allowed};
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

/// An RFC3339 timestamp `minutes` in the past — the wall-clock axis the SLA
/// arithmetic (`seconds_since` in cycle/mod.rs) reads `asked_at` on.
fn minutes_ago(minutes: i64) -> String {
    (time::OffsetDateTime::now_utc() - time::Duration::minutes(minutes))
        .format(&time::format_description::well_known::Rfc3339)
        .expect("rfc3339")
}

fn question(id: &str, ticket: &str, to: &str, body: &str, asked_at: String) -> AgentQuestion {
    AgentQuestion {
        id: id.to_owned(),
        ticket: ticket.to_owned(),
        from: "DEV-FEATURE".to_owned(),
        to: to.to_owned(),
        body: body.to_owned(),
        answer: String::new(),
        asked_at,
        answered_at: String::new(),
        forwarded: false,
        escalated: false,
        deferred: false,
    }
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

fn config_with_sla(minutes: u64) -> Config {
    let mut config = Config::default();
    config.workflow.human.question_sla_minutes = minutes;
    config
}

fn uc_with(store: Arc<MemStore>, config: Config) -> RunCycleUseCase<MemStore, SilentEngine> {
    RunCycleUseCase::new(
        store,
        Arc::new(SilentEngine),
        config,
        PathBuf::from("/tmp"),
        String::new(),
    )
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

/// The six human-gate moves `gate_promises.rs` pins to endpoints, with the
/// ticket types each edge is legal for. Escalation must never need a new edge
/// and its actor must never be able to take one of these decisions itself.
const PROMISED_GATE_EDGES: &[(&str, Status, Status, &[TicketType])] = &[
    (
        "approve → Ready",
        Status::Pending,
        Status::Ready,
        &[TicketType::Feature, TicketType::Chore],
    ),
    (
        "reject (Pending)",
        Status::Pending,
        Status::Rejected,
        &[TicketType::Feature, TicketType::Chore],
    ),
    (
        "reject / undo-as-reject (Ready)",
        Status::Ready,
        Status::Rejected,
        &[TicketType::Feature, TicketType::Chore],
    ),
    (
        "undo approval (Ready → Pending)",
        Status::Ready,
        Status::Pending,
        &[TicketType::Feature, TicketType::Chore],
    ),
    (
        "human verify",
        Status::Fixed,
        Status::Verified,
        &[TicketType::Bug],
    ),
    (
        "send back",
        Status::Fixed,
        Status::Open,
        &[TicketType::Bug],
    ),
];

/// AC1 — the waiting-human items are exactly the OPEN questions addressed to a
/// person (`@username`): agent-queue questions belong to the answering agents,
/// and an answered question is history. Both stay out of the escalation.
#[tokio::test]
async fn ac1_only_open_person_addressed_items_count_as_waiting_human_items() {
    let mut state = ProjectState::default();
    state.tickets.push(pending_designed_feature("CXC-F236A"));
    state.tickets.push(pending_designed_feature("CXC-F236B"));
    state.tickets.push(pending_designed_feature("CXC-F236C"));
    // Agent-queue question (no @): the agents answer it, not a person.
    state
        .questions
        .push(question("CXC-F236A#1", "CXC-F236A", "BA", "agent-queue", minutes_ago(120)));
    // Addressed to a person but already answered: not waiting any more.
    let mut answered = question("CXC-F236B#1", "CXC-F236B", "@carol", "resolved", minutes_ago(120));
    answered.answer = "found it".to_owned();
    state.questions.push(answered);
    // Open, person-addressed, far past the SLA: the one true escalation item.
    state
        .questions
        .push(question("CXC-F236C#1", "CXC-F236C", "@carol", "still blocked", minutes_ago(120)));

    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    uc_with(Arc::clone(&store), config_with_sla(30))
        .escalate_stale_human_questions()
        .await;

    let state = store.load().await.expect("load");
    assert!(!state.questions[0].escalated, "agent-queue questions never escalate");
    assert!(!state.questions[1].escalated, "answered questions are not waiting items");
    assert!(state.questions[2].escalated, "the open @-person question escalated");
    let msgs = state.chat_in(AGENTS_CHANNEL);
    assert_eq!(msgs.len(), 1, "exactly one escalation surfaced: {msgs:?}");
    assert!(msgs[0].body.contains("CXC-F236C#1"), "{:?}", msgs[0].body);
    assert!(!msgs[0].body.contains("CXC-F236A#1"));
    assert!(!msgs[0].body.contains("CXC-F236B#1"));
}

/// AC2 — escalation is a notification pass: it must never move a ticket along
/// ANY edge (no transition side-effects), only mark the question and post the
/// surface data. The gate-waiting tickets (ready-gate Pending, verify-gate
/// Fixed) are byte-identical before and after.
#[tokio::test]
async fn ac2_escalation_issues_no_transition_side_effects_and_moves_no_ticket() {
    let mut state = ProjectState::default();
    state.tickets.push(pending_designed_feature("CXC-F236A"));
    state.tickets.push(fixed_bug("CXC-F236B"));
    state.add_evidence("CXC-F236B", "api", "GET /health", "200 ok");
    state
        .questions
        .push(question("CXC-F236A#1", "CXC-F236A", "@carol", "soft-delete or move?", minutes_ago(120)));

    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    let before = statuses(&store).await;
    uc_with(Arc::clone(&store), config_with_sla(30))
        .escalate_stale_human_questions()
        .await;
    let after = statuses(&store).await;

    assert_eq!(before, after, "escalation moved a ticket — a transition side-effect");
    let state = store.load().await.expect("load");
    assert!(
        state.comments.is_empty(),
        "escalation must not comment on tickets: {:?}",
        state.comments
    );
    // The only surface it produces is the notification itself plus the
    // once-flag on the question — nothing a gate endpoint would consume.
    assert_eq!(state.chat_in(AGENTS_CHANNEL).len(), 1);
    assert!(state.questions[0].escalated);
}

/// AC2 — every promised human-gate edge stays a HUMAN decision: the transition
/// table grants none of them to the roles the escalation and the dev agents
/// act as (SM, DEV-FEATURE, DEV-BUG), while the human path (`Role::User`) is
/// intact for all six — the escalation can only ever route the decision to a
/// person, never perform or bypass it. Mirrors `gate_promises.rs` from the
/// escalation side.
#[test]
fn ac2_the_escalation_actor_cannot_take_any_promised_human_gate_decision() {
    for (endpoint, from, to, types) in PROMISED_GATE_EDGES {
        assert!(
            can_transition(Role::User, *from, *to),
            "{endpoint}: the human path must stay open for the escalation to route to"
        );
        for actor in [Role::Sm, Role::DevFeature, Role::DevBug] {
            assert!(
                !can_transition(actor, *from, *to),
                "{endpoint}: {actor:?} must not be able to take the gate decision itself"
            );
        }
        for &t in *types {
            assert!(
                transition_allowed(t, *from, *to),
                "{endpoint}: {t:?} lost its legal edge — the escalation must never need a new one"
            );
        }
    }
}

/// AC3 — the documented default: `question_sla_minutes` defaults to 0 and the
/// field doc pins "0 = never escalate", so an item waiting since hours is
/// left alone when the config omits the SLA.
#[tokio::test]
async fn ac3_omitted_sla_is_the_documented_never_escalate_default() {
    let config = Config::default();
    assert_eq!(
        config.workflow.human.question_sla_minutes, 0,
        "the documented default SLA is 0 = never escalate"
    );

    let mut state = ProjectState::default();
    state.tickets.push(pending_designed_feature("CXC-F236A"));
    state
        .questions
        .push(question("CXC-F236A#1", "CXC-F236A", "@carol", "blocked", minutes_ago(600)));

    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    uc_with(Arc::clone(&store), config)
        .escalate_stale_human_questions()
        .await;

    let state = store.load().await.expect("load");
    assert!(!state.questions[0].escalated, "SLA 0 = never escalate");
    assert!(
        state.chat_in(AGENTS_CHANNEL).is_empty(),
        "no notification may surface under the default"
    );
}

/// AC3 — the SLA duration comes from `workflow.human`: an item inside the
/// configured window stays untouched, an item at or past it escalates, and
/// the notification carries the item's identity.
#[tokio::test]
async fn ac3_items_within_the_configured_sla_window_stay_and_items_past_it_escalate() {
    let mut state = ProjectState::default();
    state.tickets.push(pending_designed_feature("CXC-F236A"));
    state.tickets.push(pending_designed_feature("CXC-F236B"));
    state
        .questions
        .push(question("CXC-F236A#1", "CXC-F236A", "@carol", "fresh", minutes_ago(5)));
    state
        .questions
        .push(question("CXC-F236B#1", "CXC-F236B", "@carol", "stale", minutes_ago(120)));

    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    uc_with(Arc::clone(&store), config_with_sla(30))
        .escalate_stale_human_questions()
        .await;

    let state = store.load().await.expect("load");
    assert!(!state.questions[0].escalated, "5m < 30m SLA: still inside the window");
    assert!(state.questions[1].escalated, "120m >= 30m SLA: exceeded");
    let msgs = state.chat_in(AGENTS_CHANNEL);
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].body.contains("CXC-F236B#1"), "{:?}", msgs[0].body);
    assert!(msgs[0].body.contains("CXC-F236B"), "{:?}", msgs[0].body);
    assert!(!msgs[0].body.contains("CXC-F236A#1"));
}

/// AC3 — escalation fires ONCE per item: a second pass re-escalates nothing
/// (repeats are just a second kind of spam) and the once-flag stays set.
#[tokio::test]
async fn ac3_escalation_fires_once_per_item_and_never_repeats() {
    let mut state = ProjectState::default();
    state.tickets.push(pending_designed_feature("CXC-F236A"));
    state
        .questions
        .push(question("CXC-F236A#1", "CXC-F236A", "@carol", "blocked", minutes_ago(120)));

    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    let cycle = uc_with(Arc::clone(&store), config_with_sla(30));
    cycle.escalate_stale_human_questions().await;
    cycle.escalate_stale_human_questions().await;

    let state = store.load().await.expect("load");
    assert_eq!(
        state.chat_in(AGENTS_CHANNEL).len(),
        1,
        "the second pass must not repeat the escalation"
    );
    assert_eq!(
        state
            .activity
            .iter()
            .filter(|a| a.action.contains("escalated an overdue human question"))
            .count(),
        1,
        "one activity entry, not one per pass"
    );
    assert!(state.questions[0].escalated);
}
