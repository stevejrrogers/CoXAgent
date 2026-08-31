//! CXA-F032 acceptance gate -- "Bug backlog burn-down".
//!
//! Written before implementation (TDD) so the acceptance criteria are pinned
//! as executable invariants over the state/domain types; compiles today and
//! fails only for the missing behaviour. Scope is the WHOLE bug backlog (F022
//! covered the four bugs gating F001), so every burned-down bug is any `Bug`
//! ticket in `ProjectState::tickets`.
//!
//! Red today, and why: F022 taught the agent TEST path (`RunTestUseCase`) to
//! record per-fix REGRESSION TEST evidence on `Fixed -> Verified`. AC#2
//! however applies to EVERY path that renders a Verified verdict, and the
//! HUMAN verdict paths still record no evidence at all -- the chat
//! `verify <id>` command (`RunChatReplyUseCase::human_gate_action`) and the
//! Inbox verify endpoint (`presentation::server::inbox::human_transition`)
//! both promote a bug to `Status::Verified` with only an activity log and a
//! comment. The final test drives the real chat path and fails until
//! verification records the evidence there too.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use coxagent_application::config::Language;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, StateStorePort,
};
use coxagent_application::prompts::{system_prompt, BASE, ENGINEERING_STANDARDS};
use coxagent_application::state::ProjectState;
use coxagent_application::use_cases::RunChatReplyUseCase;
use coxagent_application::PortError;
use coxagent_domain::{
    Complexity, DomainError, Priority, Role, Status, Ticket, TicketId, TicketType,
};

const REGRESSION_EVIDENCE_LABEL: &str = "REGRESSION TEST";
/// AC#2: the detail must contain a PASS marker plus proof of a clean
/// reproduction and a root cause.
const PASS_MARKER: &str = "PASS";
const CLEAN_REPRO_MARKER: &str = "reproduces";
const ROOT_CAUSE_MARKER: &str = "root cause";
// AC#2 also rejects evidence that mentions only masking talk -- "workaround",
// "symptom", "incidentally" -- instead of the proof markers: such detail
// carries none of PASS + clean reproduction + root cause and is rejected by
// the requirements above, so symptom-masking fixes cannot count as cleared.
// `ac2_symptom_masking_evidence_is_rejected` pins that with fixtures.

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

/// A backlog bug: bugs start `Open` by domain law (`Ticket::new`).
fn backlog_bug(id: &str) -> Ticket {
    Ticket::new(
        tid(id),
        TicketType::Bug,
        format!("defect {id}"),
        "repro: run the flow, observe the failure",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("bug")
}

/// The burn-down's tracked set: EVERY bug in the backlog (CXA-F032 scope --
/// F022 was only the four gating F001).
fn tracked_bugs(state: &ProjectState) -> Vec<TicketId> {
    state
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Bug)
        .map(|t| t.id().clone())
        .collect()
}

/// AC#2: does this bug carry its OWN recorded QA evidence labelled
/// "REGRESSION TEST" whose detail contains the PASS marker plus proof of a
/// clean reproduction and a root cause? Detail that mentions only masking
/// talk carries none of the proof markers and is rejected by construction.
fn has_regression_pass_evidence(state: &ProjectState, id: &TicketId) -> bool {
    state
        .ticket_evidence
        .get(&id.to_string())
        .is_some_and(|evs| {
            evs.iter().any(|e| {
                e.label.starts_with(REGRESSION_EVIDENCE_LABEL)
                    && e.detail.contains(PASS_MARKER)
                    && e.detail.contains(CLEAN_REPRO_MARKER)
                    && e.detail.contains(ROOT_CAUSE_MARKER)
            })
        })
}

/// AC#4: a re-open exists when an Open/Fixed bug shares its title with an
/// already-cleared (Verified) bug. Closure can never be declared while one
/// exists.
fn reopened_copy_exists(state: &ProjectState) -> bool {
    let verified_titles: Vec<String> = state
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Bug && matches!(t.status(), Status::Verified))
        .map(|t| t.title().to_string())
        .collect();
    state.tickets.iter().any(|t| {
        t.ticket_type() == TicketType::Bug
            && matches!(t.status(), Status::Open | Status::Fixed)
            && verified_titles.contains(&t.title().to_string())
    })
}

/// AC#1/#3: the burn-down is complete only when EVERY tracked bug has been
/// driven to `Status::Verified` via the legal transitions (the fixtures below
/// drive exactly Open -> InProgress -> Fixed -> Verified; the domain rejects
/// any shortcut), each carries its own REGRESSION TEST PASS evidence (AC#2),
/// and no re-opened copy blocks closure (AC#4). A burn-down with zero open
/// bugs (an empty backlog -- nothing was burned down) or a partial clear does
/// NOT count as done.
fn burn_down_complete(state: &ProjectState) -> bool {
    let tracked = tracked_bugs(state);
    if tracked.is_empty() {
        return false;
    }
    for id in &tracked {
        let Some(t) = state.ticket(id) else {
            return false;
        };
        if !matches!(t.status(), Status::Verified) {
            return false;
        }
        if !has_regression_pass_evidence(state, id) {
            return false;
        }
    }
    !reopened_copy_exists(state)
}

/// Record the bug's own passing root-cause regression test as QA evidence --
/// the same shape of detail production records on the agent TEST path
/// (`run_test.rs`), which is what makes that path AC#2-compliant today.
fn record_regression_pass(state: &mut ProjectState, id: &TicketId) {
    state.add_evidence(
        &id.to_string(),
        "test",
        REGRESSION_EVIDENCE_LABEL,
        "PASS on current master; regression test fails on pre-fix code and \
         reproduces cleanly; root cause fixed at source.",
    );
}

/// Drive one bug along the ONLY legal route to `Fixed` (DEV-BUG's edges).
fn drive_to_fixed(state: &mut ProjectState, id: &TicketId) {
    {
        let t = state.ticket_mut(id).expect("ticket present");
        t.transition_to(Role::DevBug, Status::InProgress)
            .expect("Open -> InProgress is a legal bug edge for DEV-BUG");
    }
    {
        let t = state.ticket_mut(id).expect("ticket present");
        t.transition_to(Role::DevBug, Status::Fixed)
            .expect("InProgress -> Fixed is a legal bug edge for DEV-BUG");
    }
}

/// Drive one bug along the full legal route Open -> InProgress -> Fixed ->
/// Verified, recording its own regression PASS evidence on the way -- exactly
/// what burning one bug down must do.
fn clear_bug(state: &mut ProjectState, id: &TicketId) {
    drive_to_fixed(state, id);
    record_regression_pass(state, id);
    let t = state.ticket_mut(id).expect("ticket present");
    t.transition_to(Role::Test, Status::Verified)
        .expect("Fixed -> Verified is a legal bug edge for TEST");
}

/// A backlog with `n` open bugs and nothing else.
fn fresh_backlog(ids: &[&str]) -> ProjectState {
    let mut s = ProjectState::default();
    for id in ids {
        s.tickets.push(backlog_bug(id));
    }
    s
}

// --- AC#1: complete only when EVERY open bug reached Verified via the legal
//     transitions; the domain rejects the shortcut past the table. ---
#[test]
fn ac1_full_burn_down_through_legal_transitions_is_complete() {
    let mut s = fresh_backlog(&["B2201", "B2202", "B2203"]);
    for id in tracked_bugs(&s) {
        clear_bug(&mut s, &id);
    }
    assert!(
        burn_down_complete(&s),
        "every bug Verified -> burn-down done"
    );
    for id in tracked_bugs(&s) {
        assert_eq!(
            s.ticket(&id).expect("present").status(),
            Status::Verified,
            "no tracked bug remains Open or Fixed"
        );
    }
    // "via the legal transitions (Open -> InProgress -> Fixed -> Verified)":
    // the domain must refuse to jump straight from Open to Verified.
    let mut s = fresh_backlog(&["B2210"]);
    let t = s.ticket_mut(&tid("B2210")).expect("present");
    assert!(
        matches!(
            t.transition_to(Role::Test, Status::Verified),
            Err(DomainError::InvalidTransition { .. })
        ),
        "Open -> Verified is not a legal edge; the burn-down path is enforced"
    );
}

// --- AC#1: a burn-down with zero open bugs (empty backlog) does NOT count as
//     done, and neither does one where "open" is zero only because every bug
//     is parked at Fixed -- unverified work is not burned down. ---
#[test]
fn ac1_zero_open_bugs_burn_down_does_not_count_as_done() {
    let empty = ProjectState::default();
    assert!(
        !burn_down_complete(&empty),
        "nothing to burn down is not a completed burn-down"
    );

    let mut s = fresh_backlog(&["B2211", "B2212"]);
    for id in tracked_bugs(&s) {
        drive_to_fixed(&mut s, &id);
    }
    assert!(
        !burn_down_complete(&s),
        "zero OPEN bugs but none Verified -> still not done"
    );
}

// --- AC#2: a Verified bug without its own recorded REGRESSION TEST PASS
//     evidence is NOT cleared -- recording the evidence is what completes it. ---
#[test]
fn ac2_verified_without_regression_evidence_is_not_cleared() {
    let mut s = fresh_backlog(&["B2221", "B2222"]);
    for id in tracked_bugs(&s) {
        drive_to_fixed(&mut s, &id);
        let t = s.ticket_mut(&id).expect("present");
        t.transition_to(Role::Test, Status::Verified)
            .expect("verify");
    }
    assert!(
        !burn_down_complete(&s),
        "Verified without its own regression PASS evidence is not cleared"
    );
    for id in tracked_bugs(&s) {
        record_regression_pass(&mut s, &id);
    }
    assert!(
        burn_down_complete(&s),
        "with each bug's own evidence recorded, the same state completes"
    );
}

// --- AC#2: evidence that mentions only masking talk (workaround / symptom /
//     incidentally) instead of PASS + clean reproduction + root cause is
//     rejected, so symptom-masking fixes cannot count as cleared. ---
#[test]
fn ac2_symptom_masking_evidence_is_rejected() {
    let masking_details = [
        // Workaround talk only -- no proof at all.
        "workaround applied; symptom gone for now",
        // Carries a PASS marker but grounds it in masking, not in a clean
        // reproduction with a root cause: exactly the symptom-mask AC#2 bans.
        "PASS observed; workaround applied and the symptom disappeared",
        // Incidental observation only.
        "noticed incidentally during a re-run; did not investigate",
    ];
    for (i, detail) in masking_details.iter().enumerate() {
        let id = format!("B223{}", i + 1);
        let mut s = fresh_backlog(&[&id]);
        clear_bug(&mut s, &tid(&id));
        // Replace the legitimate evidence with the masking-only variant.
        s.ticket_evidence.insert(id.clone(), vec![]);
        s.add_evidence(&id, "test", REGRESSION_EVIDENCE_LABEL, detail);
        assert!(
            !burn_down_complete(&s),
            "masking-only evidence must not count as cleared: {detail:?}"
        );
    }
    // The label matters too: the same proof under a different label is not
    // the required "QA evidence labelled 'REGRESSION TEST'".
    let mut s = fresh_backlog(&["B2235"]);
    clear_bug(&mut s, &tid("B2235"));
    s.ticket_evidence.insert("B2235".to_owned(), vec![]);
    s.add_evidence(
        "B2235",
        "test",
        "QA note",
        "PASS on current master; reproduces cleanly; root cause fixed at source.",
    );
    assert!(
        !burn_down_complete(&s),
        "proof under the wrong label is not the required REGRESSION TEST evidence"
    );
}

// --- AC#2: EVERY burned-down bug carries its OWN recorded evidence -- one
//     bug's proof never covers its siblings. ---
#[test]
fn ac2_each_bug_needs_its_own_recorded_evidence() {
    let mut s = fresh_backlog(&["B2241", "B2242", "B2243"]);
    for id in tracked_bugs(&s) {
        clear_bug(&mut s, &id);
    }
    // Strip two of the three: one bug's own evidence must not vouch for the
    // others.
    for id in ["B2242", "B2243"] {
        s.ticket_evidence.insert(id.to_owned(), vec![]);
    }
    assert!(
        !burn_down_complete(&s),
        "a sibling's evidence does not clear an evidence-less bug"
    );
    for id in ["B2242", "B2243"] {
        record_regression_pass(&mut s, &tid(id));
    }
    assert!(
        burn_down_complete(&s),
        "each bug's OWN evidence completes it"
    );
}

// --- AC#3: clearing only a SUBSET leaves the burn-down in progress. ---
#[test]
fn ac3_partial_clear_leaves_burn_down_in_progress() {
    let mut s = fresh_backlog(&["B2251", "B2252", "B2253"]);
    let ids = tracked_bugs(&s);
    clear_bug(&mut s, &ids[0]);
    assert!(
        !burn_down_complete(&s),
        "one of three cleared -> still in progress, nothing reports completion"
    );
}

// --- AC#3: nothing reports completion while any tracked bug remains Open or
//     Fixed rather than Verified. ---
#[test]
fn ac3_completion_blocked_while_any_bug_is_open_or_fixed() {
    let mut s = fresh_backlog(&["B2261", "B2262"]);
    let ids = tracked_bugs(&s);
    clear_bug(&mut s, &ids[0]);
    // ids[1] still Open.
    assert!(!burn_down_complete(&s), "a bug still Open blocks closure");
    drive_to_fixed(&mut s, &ids[1]);
    assert!(
        !burn_down_complete(&s),
        "a bug still Fixed (never Verified) blocks closure"
    );
}

// --- AC#4: a re-opened copy of an already-cleared bug (same title, back in
//     Open/Fixed) blocks closure until IT is verified too. ---
#[test]
fn ac4_reopened_copy_blocks_closure_until_itself_verified() {
    let mut s = fresh_backlog(&["B2271", "B2272"]);
    for id in tracked_bugs(&s) {
        clear_bug(&mut s, &id);
    }
    assert!(burn_down_complete(&s), "baseline: all cleared");

    // The cleared B2271's defect re-appears: a NEW bug ticket sharing its
    // title, back in Open.
    let copy = Ticket::new(
        tid("B2299"),
        TicketType::Bug,
        "defect B2271".to_string(),
        "re-opened copy of a cleared bug",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("copy");
    s.tickets.push(copy);
    assert!(
        !burn_down_complete(&s),
        "closure is blocked while a re-opened copy exists"
    );

    // Burning the re-open down -- legal transitions plus its own evidence --
    // is what unblocks closure.
    clear_bug(&mut s, &tid("B2299"));
    assert!(
        burn_down_complete(&s),
        "closure returns once the re-opened copy is itself verified"
    );
}

// --- AC#5: no production behaviour regresses as part of clearing bugs -- any
//     fix that depends on feature F001 still composes BASE +
//     ENGINEERING_STANDARDS + role section via system_prompt, unchanged. ---
#[test]
fn ac5_system_prompt_still_composes_base_standards_role() {
    for role in ["QA Engineer", "Business Analyst", ""] {
        let sp = system_prompt(role);
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

// ---- Production path: the HUMAN verdict route must satisfy AC#2 too ----

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

/// Engine double. A bare gate command is handled deterministically before any
/// engine call, so this is never invoked; it only satisfies the constructor.
struct NeverCalled;

#[async_trait]
impl AgentEnginePort for NeverCalled {
    fn id(&self) -> &'static str {
        "never-called"
    }
    async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
        Ok(AgentOutcome {
            exit_code: Some(0),
            stdout: "[]".to_string(),
            ..Default::default()
        })
    }
}

// ---- AC#2 driven through the PRODUCTION human verify path ----
//
// A burn-down bug may be verified by a person: the chat command
// "verify <id>" (or the Inbox verify button) moves Fixed -> Verified with the
// user's authority. AC#2 demands EVERY burned-down bug carry its own
// REGRESSION TEST PASS evidence -- so the production path that renders the
// human verdict must record it, exactly as the agent TEST path has since
// F022. Today `human_gate_action` promotes with only an activity log and a
// comment, so this assertion FAILS pre-F032 (red for the right reason).
#[tokio::test]
async fn ac2_human_verify_path_records_regression_evidence() {
    // The chat gate command only recognises ids containing '-' (the house id
    // convention, e.g. BUG-042), so this fixture uses the canonical form.
    let bug_id = "BUG-2281";
    let store = Arc::new(MemStore::default());
    {
        let mut s = store.load().await.expect("load");
        s.tickets.push(backlog_bug(bug_id));
        drive_to_fixed(&mut s, &tid(bug_id));
        store.save(&s).await.expect("save seed state");
    }

    let uc = RunChatReplyUseCase::new(
        Arc::clone(&store),
        Arc::new(NeverCalled),
        PathBuf::from("/tmp"),
        false,
        Language::En,
    );
    // Box::pin: the use-case future crossed the large-future lint threshold
    // when the governance ledger joined ProjectState (CXA-F230).
    Box::pin(uc.execute(&format!("verify {bug_id}")))
        .await
        .expect("human verify command handled");

    let s = store.load().await.expect("load post-verify");
    let id = tid(bug_id);
    assert_eq!(
        s.ticket(&id).expect("present").status(),
        Status::Verified,
        "the human verdict moves the bug to Verified"
    );
    assert!(
        has_regression_pass_evidence(&s, &id),
        "AC#2: a burn-down bug verified through the human gate must still \
         carry its own REGRESSION TEST PASS evidence -- production today \
         promotes without recording any"
    );
}
