//! CXA-F022 acceptance gate -- "Bug burn-down: clear 4 open bugs gating the
//! F001 prompt system".
//!
//! Written before implementation so its acceptance criteria are pinned as
//! executable invariants over repo state; compiles on master and fails until
//! F022 lands. A bug "gates feature F001" when its `depends_on` includes F001.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use coxagent_application::config::Config;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, StateStorePort,
};
use coxagent_application::prompts::{system_prompt, BASE, ENGINEERING_STANDARDS};
use coxagent_application::state::ProjectState;
use coxagent_application::use_cases::RunTestUseCase;
use coxagent_application::{AppError, PortError};
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};

const GATED_FEATURE: &str = "F001";
const REGRESSION_EVIDENCE_LABEL: &str = "REGRESSION TEST";
const REGRESSION_PASS_MARKER: &str = "PASS";

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

fn gating_bug(id: &str) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Bug,
        format!("prompt defect {id}"),
        format!("blocking {GATED_FEATURE}"),
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("bug");
    t.add_dependency(Role::Sa, tid(GATED_FEATURE))
        .expect("link");
    t
}

fn gating_bugs(state: &ProjectState) -> Vec<TicketId> {
    state
        .tickets
        .iter()
        .filter(|t| {
            t.ticket_type() == TicketType::Bug && t.depends_on().contains(&tid(GATED_FEATURE))
        })
        .map(|t| t.id().clone())
        .collect()
}

fn recorded_regression_pass(state: &ProjectState, id: &TicketId) -> bool {
    state
        .ticket_evidence
        .get(&id.to_string())
        .is_some_and(|evs| {
            evs.iter().any(|e| {
                e.label.starts_with(REGRESSION_EVIDENCE_LABEL)
                    && e.detail.contains(REGRESSION_PASS_MARKER)
                    && e.detail.contains("reproduces")
                    && e.detail.contains("root cause")
                    && !e.detail.contains("symptom")
                    && !e.detail.contains("workaround")
                    && !e.detail.contains("incidentally")
            })
        })
}

fn reopened_copy_exists(state: &ProjectState) -> bool {
    let verified_titles: Vec<String> = gating_bugs(state)
        .iter()
        .filter_map(|id| state.ticket(id))
        .filter(|t| matches!(t.status(), Status::Verified))
        .map(|t| t.title().to_string())
        .collect();
    state.tickets.iter().any(|t| {
        t.ticket_type() == TicketType::Bug
            && matches!(t.status(), Status::Open | Status::Fixed)
            && t.depends_on().contains(&tid(GATED_FEATURE))
            && verified_titles.contains(&t.title().to_string())
    })
}

fn burndown_complete(state: &ProjectState) -> bool {
    let all = gating_bugs(state);
    if all.is_empty() {
        return false;
    }
    for id in &all {
        let Some(t) = state.ticket(id) else {
            return false;
        };
        let verified = matches!(t.status(), Status::Verified);
        if !verified || !recorded_regression_pass(state, id) {
            return false;
        }
    }
    !reopened_copy_exists(state)
}

fn record_regression_pass(state: &mut ProjectState, id: &TicketId) {
    state.add_evidence(
        &id.to_string(),
        "regression",
        REGRESSION_EVIDENCE_LABEL,
        "PASS on current master; regression test fails on pre-fix code and \
         reproduces cleanly; root cause fixed at source.",
    );
}

/// Drive one gating bug from Open through Fixed (DEV fixes) then Verified (TEST
/// confirms) using only legal domain transitions, recording its regression-test
/// evidence along the way — exactly what F022's burn-down must do for each.
fn fix_and_verify(state: &mut ProjectState, id: &TicketId) {
    {
        let t = state.ticket_mut(id).expect("ticket present");
        t.transition_to(Role::DevBug, Status::InProgress)
            .expect("claim");
    }
    {
        let t = state.ticket_mut(id).expect("ticket present");
        t.transition_to(Role::DevBug, Status::Fixed).expect("fix");
    }
    record_regression_pass(state, id);
    {
        let t = state.ticket_mut(id).expect("ticket present");
        t.transition_to(Role::Test, Status::Verified)
            .expect("verify");
    }
}

/// A repo state with exactly four open bugs gating feature F001.
fn fresh_state() -> ProjectState {
    let mut s = ProjectState::default();
    for id in ["B1101", "B1102", "B1103", "B1104"] {
        s.tickets.push(gating_bug(id));
    }
    s
}

// --- AC#1: all four transition to verified; none remains open or fixed ---
#[test]
fn ac1_all_four_bugs_verified_and_none_open_or_fixed() {
    let mut s = fresh_state();
    for id in gating_bugs(&s) {
        fix_and_verify(&mut s, &id);
    }
    assert!(burndown_complete(&s), "all four verified -> burn-down done");
    for id in gating_bugs(&s) {
        let t = s.ticket(&id).expect("present");
        assert_eq!(t.status(), Status::Verified);
        assert!(!matches!(t.status(), Status::Open | Status::Fixed));
    }
}

// --- AC#2/#3: a Verified bug without its recorded root-cause regression PASS
//     is NOT cleared — masking symptoms must be rejected even if status moves ---
#[test]
fn ac2_ac3_verified_without_regression_evidence_is_not_cleared() {
    let mut s = fresh_state();
    for id in gating_bugs(&s) {
        let t = s.ticket_mut(&id).expect("present");
        t.transition_to(Role::DevBug, Status::InProgress)
            .expect("claim");
        t.transition_to(Role::DevBug, Status::Fixed).expect("fix");
        // NOTE: deliberately NO record_regression_pass here.
        t.transition_to(Role::Test, Status::Verified)
            .expect("verify");
    }
    // Every bug reached Verified yet none carries evidence of its own passing
    // root-cause regression test => must not count as burned down.
    assert!(!burndown_complete(&s));
}

// --- AC#4a: clearing only a SUBSET leaves the burn-down in progress ---
#[test]
fn ac4_subset_leaves_burn_down_in_progress() {
    let mut s = fresh_state();
    let ids = gating_bugs(&s);
    fix_and_verify(&mut s, &ids[0]);
    assert!(
        !burndown_complete(&s),
        "one of four cleared -> still in progress"
    );
}

// --- AC#4b: a newly re-opened COPY of an already-cleared bug keeps closure
//     from happening even when all four originals are Verified ---
#[test]
fn ac4_reopened_copy_blocks_closure() {
    let mut s = fresh_state();
    for id in gating_bugs(&s) {
        fix_and_verify(&mut s, &id);
    }
    assert!(burndown_complete(&s), "baseline all four cleared");
    // A new copy of the already-cleared B1101 re-appears, still Open, and
    // again gates F001. It shares B1101's title so it is recognized as a
    // re-open rather than fresh work.
    let mut copy = Ticket::new(
        tid("B1199"),
        TicketType::Bug,
        "prompt defect B1101".to_string(),
        format!("re-opened copy of {GATED_FEATURE} blocker"),
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("copy");
    copy.add_dependency(Role::Sa, tid(GATED_FEATURE))
        .expect("link");
    s.tickets.push(copy);
    assert!(
        !burndown_complete(&s),
        "a re-opened copy keeps closure from happening"
    );
}

// --- AC#5: the prompt-system behaviour F001 depends on (every run still
//     composes BASE + ENGINEERING_STANDARDS + role section via system_prompt)
//     is unchanged by all fixes ---
#[test]
fn ac5_system_prompt_still_composes_base_standards_role() {
    for role in ["QA Engineer", "Business Analyst", ""] {
        let sp = system_prompt(role);
        // AC#5: every run still composes BASE + ENGINEERING_STANDARDS + role
        // section via system_prompt — order and membership must be unchanged.
        assert!(sp.starts_with(BASE), "system prompt must open with BASE");
        assert!(
            sp[BASE.len()..].contains(ENGINEERING_STANDARDS),
            "BASE must be followed by ENGINEERING_STANDARDS"
        );
        if !role.is_empty() {
            assert!(
                sp.ends_with(role),
                "role section must be the trailing part of the prompt: {sp:?}"
            );
            assert!(
                sp.contains(&format!("{ENGINEERING_STANDARDS}\n\n{role}")),
                "standards then blank line then role section"
            );
        }
    }
}

// ---- Production-path check: drive the REAL TEST verification use case ----

/// In-memory store double: only load/save are required by `StateStorePort`;
/// every other method keeps its trait default.
#[derive(Default)]
struct MemStore {
    state: Mutex<ProjectState>,
}

#[async_trait]
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

/// Engine double that reports no new bugs found (empty JSON array).
struct NoNewBugs;

#[async_trait]
impl AgentEnginePort for NoNewBugs {
    fn id(&self) -> &'static str {
        "no-new-bugs"
    }
    async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
        Ok(AgentOutcome {
            exit_code: Some(0),
            stdout: "[]".to_string(),
            ..Default::default()
        })
    }
}

// ---- AC#2/#3 driven through PRODUCTION TEST verification ----
//
// This drives the real `RunTestUseCase` against a state with one Fixed gating
// bug and asserts AC#2/#3 hold end-to-end: promoting a fix to Verified must go
// hand-in-hand with recording THAT fix's own passing root-cause regression test
// in its QA evidence ("each of the 4 fixes ships with a regression test ...
// recorded in that bug's QA evidence"; "running its regression test on current
// master passes"). Today `run_test.rs` promotes Fixed -> Verified without ever
// recording such per-fix evidence, so this assertion FAILS pre-F022 (red for
// the right reason) and goes green once verification records it.
#[tokio::test]
async fn ac2_ac3_production_verification_records_regression_evidence() {
    let store = Arc::new(MemStore::default());
    {
        let mut s = store.load().await.expect("load");
        // Feature F001 exists; a gating bug legitimately depends on it.
        s.tickets.push(
            Ticket::new(
                tid(GATED_FEATURE),
                TicketType::Feature,
                "F001 prompt system",
                "the prompt system",
                Priority::High,
                Complexity::Large,
                false,
            )
            .expect("feature"),
        );
        let mut b = gating_bug("B1101");
        b.transition_to(Role::DevBug, Status::InProgress)
            .expect("claim");
        b.transition_to(Role::DevBug, Status::Fixed).expect("fix");
        s.tickets.push(b);
        store.save(&s).await.expect("save seed state");
    }

    let uc = RunTestUseCase::new(
        Arc::clone(&store),
        Arc::new(NoNewBugs),
        Config::default(),
        PathBuf::from("/tmp"),
    );
    let res: Result<Vec<TicketId>, AppError> = uc.execute().await;
    res.expect("test run succeeded");

    let s = store.load().await.expect("load post-test");
    let id = tid("B1101");
    assert_eq!(
        s.ticket(&id).expect("present").status(),
        Status::Verified,
        "TEST promotes a fixed bug to Verified"
    );
    assert!(
        recorded_regression_pass(&s, &id),
        "AC#2/#3: a Verified burn-down bug must have its own root-cause \
         regression-test PASS recorded in QA evidence -- production today \
         promotes without recording it"
    );
}
