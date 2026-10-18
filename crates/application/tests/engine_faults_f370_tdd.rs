//! TDD tests for CXA-F370 — Engine-side failures never consume ticket
//! fail-attempts: classify, auto-retry, no human un-hold.
//!
//! Acceptance criteria encoded here, verbatim from the ticket:
//! - AC1: a failure whose decisive line matches the shared engine-infra
//!   classifier (UnknownError, timed out, unavailable, credential/auth-dead)
//!   does not increment the ticket's fail-attempts and the ticket returns to
//!   ready for automatic retry.
//! - AC2: task-side failures (tests failed, build broke, gate refused) count
//!   exactly as today and 3 strikes still auto-hold.
//! - AC3: one classifier function shared by the runner circuit-breaker and
//!   the attempt counter (no duplicated pattern lists), pure over the failure
//!   snapshot, with a truth-table unit test covering both classes and the
//!   ambiguous-defaults-to-task-side rule.
//! - AC4: an engine-side failure shows in the Team view Engine health card
//!   (`engine_incidents`) rather than in the ticket's failure count.
//! - AC5 (regression): fixtures replaying the F341/F354 hold scenarios end
//!   with the tickets still ready and zero attempts consumed.
//!
//! Every fixture is built from data the codebase actually has:
//! - `UnknownError` — the verbatim error-event shape `opencode.rs`
//!   `extract_error` surfaces from a dead provider
//!   (`opencode UnknownError: Unexpected server error…`).
//! - `timed out` — the verbatim timeout errors the real adapters return
//!   (`PortError::Backend("opencode timed out")` in `opencode.rs`, `claude.rs`,
//!   `copilot.rs`).
//! - credential/auth-dead — the verbatim CLI outage lines already quoted in
//!   `faults.rs`'s own tests.
//! - The F341/F354 hold scenarios — the project's own recorded history: both
//!   tickets' WIP checkpoints read "parked by slot hygiene (engine died
//!   mid-edit)", i.e. engine-side deaths charged against innocent tickets.
//!
//! What FAILS before implementation: AC1/AC4/AC5 (UnknownError, timed out and
//! unavailable are not classified engine-side today, so they consume
//! attempts and open no incident) and the new truth-table rows. What PINS
//! today's preserved behaviour: AC2 and the shared-classifier references —
//! the F238 file's convention for "must keep holding" criteria.
//!
//! Note for the implementer (flagged, not touched — TDD discipline): AC1
//! reclassifies `timed out` as engine-side, which supersedes the existing
//! `"claude timed out"` row in the `faults.rs` unit test
//! `genuine_task_failures_are_not_infra`; that pre-existing test will need
//! updating in the implementation commit.
//!
//! All tests are PURE over the state/domain types that exist today:
//! `ProjectState` (`ticket_fail_attempts`, `engine_incidents`,
//! `attempt_failures`, `comments`), the `Ticket` aggregate, `AgentOutcome`
//! (the failure snapshot) and `RunDevUseCase` — in-memory store, no network,
//! no host harness, no fake HTTP server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use coxagent_application::config::Config;
use coxagent_application::faults::is_infra_fault;
use coxagent_application::metrics::agent_evals;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, StateStorePort,
};
use coxagent_application::selection::next_ready_feature;
use coxagent_application::use_cases::{DevMode, RunDevUseCase};
use coxagent_application::PortError;
use coxagent_domain::{
    Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Doubles — pure, in-memory. The same world `run_dev/mod.rs`'s own tests use.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MemStore {
    state: Mutex<coxagent_application::state::ProjectState>,
}

#[async_trait]
impl StateStorePort for MemStore {
    async fn load(&self) -> Result<coxagent_application::state::ProjectState, PortError> {
        Ok(self.state.lock().unwrap().clone())
    }
    async fn save(&self, s: &coxagent_application::state::ProjectState) -> Result<(), PortError> {
        s.validate().map_err(PortError::Corrupt)?;
        *self.state.lock().unwrap() = s.clone();
        Ok(())
    }
}

/// The one death this engine commits per run, verbatim from the real
/// adapters' failure surface (see the file header for provenance).
#[derive(Clone, Copy)]
enum Death {
    /// The adapter's wall-clock kill: `PortError::Backend("<cli> timed out")`.
    Timeout,
    /// A dead provider surfacing as opencode's error event, which
    /// `extract_error` folds into the outcome's stderr.
    UnknownError,
    /// The provider answers with an outage line naming no task detail.
    Unavailable,
    /// The engine ANSWERED and the work was wrong — task-side.
    TaskFailure,
}

struct DyingEngine {
    death: Death,
}

fn failed_outcome(stderr: &str) -> AgentOutcome {
    AgentOutcome {
        stderr: stderr.to_owned(),
        exit_code: Some(1),
        ..AgentOutcome::default()
    }
}

#[async_trait]
impl AgentEnginePort for DyingEngine {
    fn id(&self) -> &'static str {
        "opencode"
    }
    async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
        match self.death {
            Death::Timeout => Err(PortError::Backend("opencode timed out".to_owned())),
            Death::UnknownError => Ok(failed_outcome(
                "opencode UnknownError: Unexpected server error while generating",
            )),
            Death::Unavailable => Ok(failed_outcome("service unavailable")),
            Death::TaskFailure => Ok(failed_outcome("test result: FAILED. 3 passed; 1 failed")),
        }
    }
}

// ---------------------------------------------------------------------------
// Fixtures — a genuinely Ready feature (aggregate constructor path, so every
// invariant holds by construction), then one DEV pass per call.
// ---------------------------------------------------------------------------

fn ready_feature(id: &str, title: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Feature,
        title.to_owned(),
        format!("replay fixture for {id}"),
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    t.set_technical_design(Role::Sa, TechnicalDesign::default())
        .expect("design attached");
    t.transition_to(Role::Sa, Status::Ready).expect("ready");
    t
}

fn world(
    id: &str,
    title: &str,
    death: Death,
) -> (Arc<MemStore>, RunDevUseCase<MemStore, DyingEngine>) {
    let store = Arc::new(MemStore {
        state: Mutex::new(coxagent_application::state::ProjectState {
            tickets: vec![ready_feature(id, title)],
            ..coxagent_application::state::ProjectState::default()
        }),
    });
    let uc = RunDevUseCase::new(
        Arc::clone(&store),
        Arc::new(DyingEngine { death }),
        Config::default(),
        PathBuf::from("/tmp"),
        DevMode::Feature,
    );
    (store, uc)
}

/// One DEV pass against the dying engine. The run errors — what matters is
/// what the failure LADDER recorded, which every assertion below reads.
/// Boxed: the DEV-pass future is huge off the test frame.
async fn one_pass(uc: &RunDevUseCase<MemStore, DyingEngine>) {
    assert!(
        Box::pin(uc.execute()).await.is_err(),
        "the engine dies every run; the pass must surface the failure"
    );
}

// ---------------------------------------------------------------------------
// AC1 — engine-side deaths never increment fail-attempts; the ticket returns
// to ready for automatic retry. FAILS today: `is_infra_fault` matches none of
// the three new classes, so today each death consumes an attempt.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac1_an_engine_timeout_death_consumes_zero_attempts_and_returns_the_ticket_to_ready() {
    let (store, uc) = world(
        "CXA-F370-TMO",
        "Engine-side deaths are not ticket failures",
        Death::Timeout,
    );
    one_pass(&uc).await;

    let s = store.load().await.unwrap();
    assert!(
        !s.ticket_fail_attempts.contains_key("CXA-F370-TMO"),
        "a timeout is the ENGINE dying — fail-attempts must stay empty, got {:?}",
        s.ticket_fail_attempts
    );
    assert_eq!(s.tickets[0].status(), Status::Ready, "back in the queue");
    assert!(s.tickets[0].claimed_by().is_none(), "claim released");
    assert_eq!(
        next_ready_feature(&s).map(|i| i.as_str().to_owned()),
        Some("CXA-F370-TMO".to_owned()),
        "ready for AUTOMATIC retry: the selector picks it again with no human step"
    );
}

#[tokio::test]
async fn ac1_an_unknown_error_death_consumes_zero_attempts_and_returns_the_ticket_to_ready() {
    let (store, uc) = world(
        "CXA-F370-UNK",
        "Engine-side deaths are not ticket failures",
        Death::UnknownError,
    );
    one_pass(&uc).await;

    let s = store.load().await.unwrap();
    assert!(
        !s.ticket_fail_attempts.contains_key("CXA-F370-UNK"),
        "UnknownError is a dead provider, not the ticket's fault: {:?}",
        s.ticket_fail_attempts
    );
    assert_eq!(s.tickets[0].status(), Status::Ready);
    assert_eq!(
        next_ready_feature(&s).map(|i| i.as_str().to_owned()),
        Some("CXA-F370-UNK".to_owned()),
    );
}

#[tokio::test]
async fn ac1_an_unavailable_death_consumes_zero_attempts_and_returns_the_ticket_to_ready() {
    let (store, uc) = world(
        "CXA-F370-UNA",
        "Engine-side deaths are not ticket failures",
        Death::Unavailable,
    );
    one_pass(&uc).await;

    let s = store.load().await.unwrap();
    assert!(
        !s.ticket_fail_attempts.contains_key("CXA-F370-UNA"),
        "an unavailable provider is engine-side: {:?}",
        s.ticket_fail_attempts
    );
    assert_eq!(s.tickets[0].status(), Status::Ready);
    assert_eq!(
        next_ready_feature(&s).map(|i| i.as_str().to_owned()),
        Some("CXA-F370-UNA".to_owned()),
    );
}

// ---------------------------------------------------------------------------
// AC2 — task-side failures count exactly as today and 3 strikes still
// auto-hold. PINS today's ladder: the engine ANSWERED and the work was wrong.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac2_task_side_failures_count_as_today_and_three_strikes_still_auto_hold() {
    let (store, uc) = world(
        "CXA-F370-TASK",
        "A ticket the engine builds wrongly",
        Death::TaskFailure,
    );
    for _ in 0..3 {
        one_pass(&uc).await;
    }

    let s = store.load().await.unwrap();
    assert_eq!(
        s.ticket_fail_attempts.get("CXA-F370-TASK"),
        Some(&3),
        "tests failed = the ticket's fault: every strike counts, exactly as today"
    );
    assert_eq!(
        s.attempt_failures("CXA-F370-TASK").len(),
        3,
        "the structured attempt log records each task-side strike"
    );
    assert_eq!(
        agent_evals(&s).parked,
        1,
        "3 strikes auto-hold: the ticket reads as PARKED in the team's evals"
    );
    assert!(
        s.comments
            .iter()
            .any(|c| c.ticket.as_deref() == Some("CXA-F370-TASK")
                && c.body.contains("PARKED after 3 failed attempts")),
        "the visible hold note names the ticket and the strike count"
    );
    assert!(
        s.engine_incidents.is_empty(),
        "the engine answered and was wrong — that is NOT an engine incident"
    );
}

// ---------------------------------------------------------------------------
// AC3 — ONE classifier, truth-table over the failure snapshot, ambiguous
// defaults to task-side. The engine-side rows for UnknownError / timed out /
// unavailable FAIL today (the missing behaviour); the task-side rows and the
// ambiguous default PIN the rule that must survive the implementation.
// ---------------------------------------------------------------------------

/// The failure snapshot (`AgentOutcome`) → its decisive line, exactly the
/// derivation the failure ladder feeds the classifier.
fn decisive_line(stderr: &str, stdout: &str) -> String {
    AgentOutcome {
        stderr: stderr.to_owned(),
        stdout: stdout.to_owned(),
        exit_code: Some(1),
        ..AgentOutcome::default()
    }
    .failure_detail()
}

#[test]
fn ac3_truth_table_both_classes_with_ambiguous_defaulting_to_task_side() {
    // ENGINE-SIDE (never consumes an attempt): the four AC1 classes plus the
    // verbatim outage lines this classifier already owns.
    let engine_side: Vec<(String, &str)> = vec![
        (String::new(), "empty snapshot — every observed outage surfaced blank"),
        (
            decisive_line(
                "API Error: 401 OAuth access token has been revoked.",
                "",
            ),
            "credential/auth-dead, verbatim claude CLI",
        ),
        (
            decisive_line("", "starting\nFailed to authenticate: OAuth session expired and could not be refreshed"),
            "auth-dead riding on stdout — the decisive line is the failure",
        ),
        (
            PortError::Backend("opencode timed out".to_owned()).to_string(),
            "timed out — the adapter's wall-clock kill, Err path",
        ),
        (
            decisive_line("claude timed out", ""),
            "timed out — outcome path, same class",
        ),
        (
            decisive_line(
                "opencode UnknownError: Unexpected server error while generating",
                "",
            ),
            "UnknownError — dead provider, verbatim extract_error shape",
        ),
        (
            decisive_line("service unavailable", ""),
            "unavailable — provider outage",
        ),
        (
            decisive_line("quota exceeded, retry later", ""),
            "capacity wall — already engine-side, must stay",
        ),
    ];
    for (line, why) in &engine_side {
        assert!(
            is_infra_fault(line),
            "ENGINE-SIDE row ({why}) classified task-side: {line:?}"
        );
    }

    // TASK-SIDE (counts exactly as today): the engine answered and the WORK
    // was wrong. Includes the ambiguous row: a decisive line that matches
    // NEITHER class defaults to task-side.
    let task_side: Vec<(String, &str)> = vec![
        (
            decisive_line("test result: FAILED. 3 passed; 1 failed", ""),
            "tests failed",
        ),
        (
            decisive_line("error[E0308]: mismatched types", ""),
            "build broke",
        ),
        (
            decisive_line("added clippy errors (37 -> 40)", ""),
            "gate refused",
        ),
        (
            decisive_line("the patch did not apply cleanly", ""),
            "ambiguous — defaults to task-side",
        ),
    ];
    for (line, why) in &task_side {
        assert!(
            !is_infra_fault(line),
            "TASK-SIDE row ({why}) classified engine-side — innocent tickets would stop being counted: {line:?}"
        );
    }
}

/// The one-classifier rule: the runner circuit-breaker AND the attempt
/// counter consult `faults` — no local copy of the pattern lists. PINS the
/// sharing that must survive (and grow by exactly zero new lists).
#[test]
fn ac3_the_attempt_counter_and_the_runner_breaker_consult_one_shared_classifier() {
    let src = |rel: &str| {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/").to_owned() + rel)
            .unwrap_or_else(|e| panic!("read {rel}: {e}"))
    };
    let runner = src("src/use_cases/runner.rs");
    let counter = src("src/use_cases/run_dev/failures.rs");
    let faults = src("src/faults.rs");

    assert!(
        faults.contains("pub fn is_infra_fault"),
        "the shared classifier lives in the faults module"
    );
    assert!(
        runner.contains("faults::is_infra_fault"),
        "the runner's circuit breaker must consult the shared classifier"
    );
    assert!(
        counter.contains("faults::is_infra_fault"),
        "the attempt counter must consult the shared classifier"
    );
    // No duplicated pattern lists: neither call site carries its own copy of
    // the infra patterns (the giveaway literals of that list).
    for (name, file) in [
        ("runner.rs", runner.as_str()),
        ("failures.rs", counter.as_str()),
    ] {
        for pattern in ["\"401\"", "\"spend limit\"", "\"sandbox_apply\""] {
            assert!(
                !file.contains(pattern),
                "{name} must not carry its own infra pattern ({pattern}) — one list, in faults.rs"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// AC4 — an engine-side failure shows in the Team view Engine health card
// (`engine_incidents` — the field `renderEngineHealth` renders) rather than
// in the ticket's failure count. FAILS today: an UnknownError death opens no
// incident and consumes an attempt.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac4_an_engine_side_failure_lands_in_engine_incidents_not_in_the_ticket_failure_count() {
    let (store, uc) = world(
        "CXA-F370-INC",
        "Engine-side deaths are not ticket failures",
        Death::UnknownError,
    );
    one_pass(&uc).await;

    let s = store.load().await.unwrap();
    assert_eq!(
        s.engine_incidents.len(),
        1,
        "the outage is VISIBLE, not a log line"
    );
    let inc = &s.engine_incidents[0];
    assert_eq!(inc.engine, "opencode", "which engine is down");
    assert_eq!(inc.role, "DevFeature", "who hit it first");
    assert_eq!(inc.hits, 1, "one failed run so far");
    assert!(
        inc.reason.contains("UnknownError"),
        "the card carries the decisive line: {:?}",
        inc.reason
    );
    assert!(
        !s.ticket_fail_attempts.contains_key("CXA-F370-INC")
            && s.attempt_failures("CXA-F370-INC").is_empty(),
        "and it is NOT in the ticket's failure count — that is the whole point"
    );
    assert!(
        s.activity
            .iter()
            .any(|a| a.action.contains("attempt not counted")),
        "the trail attributes the non-count, the same data the card's hover shows"
    );
}

// ---------------------------------------------------------------------------
// AC5 — regression: fixtures replaying the F341/F354 hold scenarios end with
// the tickets still ready and zero attempts consumed. Both tickets' real WIP
// checkpoints read "parked by slot hygiene (engine died mid-edit)" — engine
// deaths charged against innocent tickets. Fails today: the timeout consumes
// attempts, and three strikes would auto-hold the ticket again.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac5_replaying_the_f341_f354_engine_died_mid_edit_scenarios_ends_ready_with_zero_attempts()
{
    // CXA-F341 (SM status digest): the engine died mid-edit — a timeout.
    let (store_f341, uc_f341) = world("CXA-F341", "SM status digest", Death::Timeout);
    for _ in 0..3 {
        one_pass(&uc_f341).await;
    }
    // CXA-F354 (E2EE DMs): the same death, via a dead provider's error event.
    let (store_f354, uc_f354) = world("CXA-F354", "E2EE direct messages", Death::UnknownError);
    for _ in 0..3 {
        one_pass(&uc_f354).await;
    }

    for (store, id) in [(store_f341, "CXA-F341"), (store_f354, "CXA-F354")] {
        let s = store.load().await.unwrap();
        assert!(
            !s.ticket_fail_attempts.contains_key(id),
            "{id}: zero attempts consumed across the whole replay — got {:?}",
            s.ticket_fail_attempts
        );
        let t = s.tickets.iter().find(|t| t.id().as_str() == id).unwrap();
        assert_eq!(t.status(), Status::Ready, "{id} still ready");
        assert!(t.claimed_by().is_none(), "{id} claim released every time");
        assert_eq!(
            next_ready_feature(&s).map(|i| i.as_str().to_owned()),
            Some(id.to_owned()),
            "{id} is still first-class automatic-retry work"
        );
        assert!(
            !s.comments.iter().any(|c| c.body.contains("PARKED after 3")),
            "{id} must never auto-hold on engine deaths"
        );
        assert_eq!(
            agent_evals(&s).parked,
            0,
            "{id}: the team's parked count stays clean — nobody has to un-hold anything"
        );
        assert_eq!(
            agent_evals(&s).failed_attempts,
            0,
            "{id}: zero attempts consumed, so churn metrics stay honest"
        );
        assert!(
            !s.engine_incidents.is_empty(),
            "{id}: the deaths went where people look — the engine health card"
        );
    }
}
