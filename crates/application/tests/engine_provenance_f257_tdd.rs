//! TDD tests for CXA-F257 — Engine & model provenance per agent step,
//! surfaced at the human verify gate.
//!
//! Written before implementation so the ticket's acceptance criteria are
//! pinned as executable tests over the state/domain types the codebase has
//! today. Every fixture is built through the legal domain aggregate API and
//! the exact records the production paths already write (run_dev's activity
//! steps, the state-store save/load boundary, inbox.rs send-back) — no
//! server, no host harness, no network port, no fabricated persisted shape.
//!
//! AC → test map (criteria quoted verbatim from the ticket):
//! - AC1 ("Every agent step shown on a ticket's activity displays the engine
//!   and model that executed it, visible to the verifying human before they
//!   approve"):
//!   [`ac1_every_step_shown_on_the_tickets_activity_carries_the_engine_and_model_that_executed_it`]
//! - AC2 ("A run that failed over mid-step shows each engine/model attempt
//!   in order (primary then fallback), not only the final one"):
//!   [`ac2_a_failed_over_run_shows_each_engine_model_attempt_in_order_primary_then_fallback`]
//! - AC3 ("When an engine cannot report a model id, the surface shows the
//!   engine name with an explicit 'model unknown' marker instead of an empty
//!   field"):
//!   [`ac3_an_engine_without_a_model_id_shows_the_engine_with_an_explicit_model_unknown_marker`]
//! - AC4 ("Steps re-executed after crash recovery keep provenance for both
//!   the pre-crash and post-crash executions"):
//!   [`ac4_steps_reexecuted_after_crash_recovery_keep_provenance_for_both_executions`],
//!   [`guard_step_provenance_round_trips_the_store_and_legacy_snapshots_load_clean`]
//! - AC5 ("Send-back cycles record provenance per attempt so the reviewer
//!   can see which model produced each version of the work"):
//!   [`ac5_send_back_cycles_record_provenance_per_attempt_so_each_version_names_its_model`]
//!
//! WHY THE PROVENANCE IS A PER-TICKET STATE RECORD (grounded, not guessed):
//! the criteria all demand per-step engine/model history that outlives the
//! moment of the run — AC2 forbids the last-wins overwrite
//! (`Spend::engine_by_role` keeps only the FINAL engine today, exactly the
//! "only the final one" anti-pattern AC2 names), AC4 demands the pre-crash
//! record survive, AC5 demands per-cycle history across send-backs. The
//! activity feed (`ActivityEntry`) is a global, 60-entry bounded ring with
//! no engine/model fields — it cannot carry this. The bounded per-ticket
//! append-only log (`ticket_failures` + `record_attempt_failure`) is the
//! house shape for exactly this kind of record.
//!
//! THE DATA ALREADY EXISTS — why no ASK is raised: every engine adapter
//! holds the model selection it was built with (`ClaudeEngine::new(choice.
//! model)`, `OpencodeEngine::new(choice.model)`, `CopilotEngine`,
//! `HermesEngine` — all constructed from `EngineChoice.model`) and the
//! engine stack already stamps the winning CLI onto the outcome
//! (`AgentOutcome.engine`, "stamped by FailoverEngine"). The crash-recovery
//! path exists (`Ticket::release_claim(Role::System)` returns a claim
//! orphaned by a crashed run to the work queue), and the send-back path
//! exists (`POST …/send-back` → `Fixed -> Open` + `InterventionKind::
//! VerifySendBack`). What is missing — and what this ticket declares — is
//! the record type carrying it per step, the append-only recording, and the
//! render decision. Every fixture below is therefore buildable from real
//! data; nothing is fabricated.
//!
//! NAMES, AND WHERE THEY COME FROM (pinned so implementer and reviewer share
//! one contract; every name is the ticket's own word or a house precedent):
//! * `StepProvenance` — the ticket's own words ("Engine & model provenance
//!   per agent step"). Fields `at`/`role`/`action` are `ActivityEntry`'s own
//!   fields (AC1 names the step "shown on a ticket's activity" — the step IS
//!   an activity item; `role` uses the exact dashboard labels run_dev
//!   writes, `DEV-BUG`/`DEV-FEATURE`); `attempts` carries the engine/model
//!   attempts in run order, primary first (AC2's own words: "primary then
//!   fallback").
//! * `EngineAttempt` — AC2's own word ("each engine/model attempt").
//!   `engine: String` is `AgentOutcome.engine`'s own word ("the engine CLI
//!   that ACTUALLY produced this outcome"); `model: Option<String>` is
//!   AC3's own case split ("when an engine cannot report a model id") —
//!   `None` is the explicit unknown, never an empty string.
//! * `ProjectState::ticket_step_provenance` — the per-ticket additive map,
//!   `#[serde(default)]` exactly like the `ticket_failures` precedent (no
//!   schema bump; pre-change snapshots load to an empty map).
//! * `record_step_provenance` / `step_provenance` — the
//!   `record_attempt_failure` / `attempt_failures` house pair; append-only
//!   and bounded, which is what makes AC4's pre-crash record and AC5's
//!   per-cycle history survive (oldest first, like `attempt_failures`).
//! * `engine_provenance::attempt_label(&EngineAttempt) -> String` — AC3's
//!   render: the engine name with the explicit `model unknown` marker when
//!   the model is absent — the `forensics::provenance_label` precedent
//!   (CXA-F241), whose unknown state renders the literal "provenance
//!   unknown". The marker words pin the contract; the separator style is the
//!   implementer's.
//!
//! REQUIRED SURFACE this suite compiles against:
//! - `coxagent_application::state::{EngineAttempt, StepProvenance}` (fields
//!   above) deriving `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`
//!   as every state record does.
//! - `ProjectState`: `#[serde(default)] pub ticket_step_provenance:
//!   std::collections::BTreeMap<String, Vec<StepProvenance>>` +
//!   `record_step_provenance(&mut self, ticket: &str, step: StepProvenance)`
//!   + `#[must_use] step_provenance(&self, ticket: &str) ->
//!   &[StepProvenance]`.
//! - `coxagent_application::engine_provenance` (registered in lib.rs, a pure
//!   module like `forensics`/`dependency_radar`) exposing `attempt_label`.
//!
//! RED STATE: none of that surface exists yet, so this target fails to
//! compile — for a declare-a-surface ticket the unresolved names ARE the
//! missing behaviour, exactly as a failing assertion is for a behaviour
//! inside an existing type (the artifact_registry_f224_tdd.rs and
//! forensics_f241_tdd.rs precedents). Once the surface above lands, the
//! target compiles and every test below fails only if the behaviour it pins
//! is wrong or missing.
//!
//! The rendered pixels themselves (the verify card's engine/model badges,
//! click → detail) are gated end-to-end by the e2e playwright suite per
//! AGENTS.md; the data those pixels consume is pinned here over the real
//! types.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::engine_provenance::attempt_label;
use coxagent_application::state::{
    EngineAttempt, InterventionRecord, ProjectState, StepProvenance,
};
use coxagent_domain::{
    Complexity, InterventionKind, Priority, Role, Status, Ticket, TicketId, TicketType,
};

// ---------------------------------------------------------------------------
// Fixtures — built ONLY through the domain aggregate's legal API and the
// exact records the production paths already write.
// ---------------------------------------------------------------------------

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

/// A bug walked to `Fixed` along the only legal edges (`Open -> InProgress ->
/// Fixed`, DEV-BUG's edges) — the state the human verify gate inspects
/// (`Fixed -> Verified` IS the verify gate), i.e. AC1's "before they
/// approve".
fn bug_at_fixed(id: &str) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Bug,
        format!("bug {id}"),
        "fixture",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("ticket");
    t.transition_to(Role::DevBug, Status::InProgress)
        .expect("claim");
    t.transition_to(Role::DevBug, Status::Fixed).expect("fix");
    t
}

/// One engine/model attempt — the data the engine stack already holds
/// (`AgentOutcome.engine` + the adapter's model selection). `None` model is
/// AC3's "engine cannot report a model id" case.
fn attempt(engine: &str, model: Option<&str>) -> EngineAttempt {
    EngineAttempt {
        engine: engine.to_owned(),
        model: model.map(str::to_owned),
    }
}

/// One agent step as the activity shows it (`ActivityEntry`'s own field
/// shapes: RFC3339 `at`, dashboard role label, action prose) with the
/// engine/model attempts that executed it, in run order.
fn step(at: &str, role: &str, action: &str, attempts: Vec<EngineAttempt>) -> StepProvenance {
    StepProvenance {
        at: at.to_owned(),
        role: role.to_owned(),
        action: action.to_owned(),
        attempts,
    }
}

/// One send-back decision exactly as `POST …/send-back` writes it
/// (inbox.rs): `Fixed -> Open` by the human gate, the activity entry, the
/// comment and the `VerifySendBack` intervention record.
fn send_back(s: &mut ProjectState, id: &str, at: &str, by: &str, reason: &str) {
    s.tickets[0]
        .transition_to(Role::User, Status::Open)
        .expect("send back");
    s.activity
        .push(activity(at, "USER", "verification refused", id));
    s.post_comment(
        by,
        &format!("↩️ {id} sent back by @{by}: {reason}"),
        Some(id.to_owned()),
    );
    s.governance_interventions.push(InterventionRecord {
        kind: InterventionKind::VerifySendBack,
        ticket: id.to_owned(),
        area: Some(TicketType::Bug),
        by: by.to_owned(),
        at: at.to_owned(),
    });
}

/// An activity entry with the field shapes `ActivityEntry` already has (the
/// feed is included in fixtures where production writes one, so the provenance
/// records sit beside the exact data the verify card already reads).
fn activity(
    at: &str,
    agent: &str,
    action: &str,
    ticket: &str,
) -> coxagent_application::state::ActivityEntry {
    coxagent_application::state::ActivityEntry {
        at: at.to_owned(),
        agent: agent.to_owned(),
        action: action.to_owned(),
        ticket: Some(ticket.to_owned()),
    }
}

// ---------------------------------------------------------------------------
// AC1 — every agent step on the ticket's activity shows engine + model,
// visible to the verifying human before they approve.
// ---------------------------------------------------------------------------

#[test]
fn ac1_every_step_shown_on_the_tickets_activity_carries_the_engine_and_model_that_executed_it() {
    const ID: &str = "CXA-B257";
    let mut s = ProjectState {
        tickets: vec![bug_at_fixed(ID)],
        ..ProjectState::default()
    };
    // The agent steps a DEV-BUG run on this ticket actually makes (run_dev):
    // the implementation pass and the agent's own pre-handoff diff review.
    // Both are engine runs; both land on the ticket's activity while the
    // reviewer decides at the verify gate.
    s.record_step_provenance(
        ID,
        step(
            "2026-08-30T10:00:00Z",
            "DEV-BUG",
            "started implementing",
            vec![attempt("claude", Some("opus"))],
        ),
    );
    s.record_step_provenance(
        ID,
        step(
            "2026-08-30T10:09:00Z",
            "DEV-BUG",
            "reviewed its own diff",
            vec![attempt("claude", Some("opus"))],
        ),
    );

    // The reviewer has NOT approved yet: the verify gate is the
    // `Fixed -> Verified` edge, so the provenance must already be visible
    // while the ticket sits at `Fixed`.
    let t = s.ticket(&tid(ID)).expect("ticket on the verify gate");
    assert_eq!(
        t.status(),
        Status::Fixed,
        "the provenance is inspected BEFORE the human approves (Fixed -> Verified)"
    );

    let steps = s.step_provenance(ID);
    assert_eq!(
        steps.len(),
        2,
        "every agent step the ticket's activity shows is recorded: {steps:?}"
    );
    for st in steps {
        assert!(
            !st.attempts.is_empty(),
            "a step without its engine attempt can display nothing: {st:?}"
        );
        for a in &st.attempts {
            assert!(
                !a.engine.is_empty(),
                "the engine that executed the step is carried: {a:?}"
            );
            let label = attempt_label(a);
            assert!(
                label.contains(&a.engine),
                "the surface displays the engine that executed the step: {label:?} vs {a:?}"
            );
            if let Some(m) = &a.model {
                assert!(
                    !m.is_empty() && label.contains(m.as_str()),
                    "the surface displays the model that executed the step: {label:?} vs {a:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AC2 — failover mid-step: every attempt in order, not only the final one.
// ---------------------------------------------------------------------------

#[test]
fn ac2_a_failed_over_run_shows_each_engine_model_attempt_in_order_primary_then_fallback() {
    const ID: &str = "CXA-B258";
    let mut s = ProjectState {
        tickets: vec![bug_at_fixed(ID)],
        ..ProjectState::default()
    };
    // One step whose run hit a quota wall on the primary and finished on the
    // fallback (FailoverEngine's primary-then-fallback order). The record
    // must keep BOTH — `Spend::engine_by_role`'s last-wins stamp is exactly
    // the "only the final one" loss AC2 forbids.
    s.record_step_provenance(
        ID,
        step(
            "2026-08-30T11:00:00Z",
            "DEV-BUG",
            "started implementing",
            vec![
                attempt("claude", Some("opus")),
                attempt("opencode", Some("bizbrain/Qwen3.6-35B-A3B-thinking")),
            ],
        ),
    );

    let steps = s.step_provenance(ID);
    assert_eq!(steps.len(), 1, "one step, one record: {steps:?}");
    let attempts = &steps[0].attempts;
    assert_eq!(
        attempts.len(),
        2,
        "each engine/model attempt is kept, not only the final one: {attempts:?}"
    );
    assert_eq!(
        attempts[0].engine, "claude",
        "the primary attempt comes first: {attempts:?}"
    );
    assert_eq!(
        attempts[0].model.as_deref(),
        Some("opus"),
        "the primary's model is carried: {attempts:?}"
    );
    assert_eq!(
        attempts[1].engine, "opencode",
        "the fallback attempt comes second: {attempts:?}"
    );
    assert_eq!(
        attempts[1].model.as_deref(),
        Some("bizbrain/Qwen3.6-35B-A3B-thinking"),
        "the fallback's model is carried: {attempts:?}"
    );
}

// ---------------------------------------------------------------------------
// AC3 — an engine without a model id renders explicitly, never an empty field.
// ---------------------------------------------------------------------------

#[test]
fn ac3_an_engine_without_a_model_id_shows_the_engine_with_an_explicit_model_unknown_marker() {
    // The unknown case: the engine name is shown WITH an explicit marker.
    let unknown = attempt("claude", None);
    let label = attempt_label(&unknown);
    assert!(
        !label.trim().is_empty(),
        "an unknown model must not render an empty field: {label:?}"
    );
    assert!(
        label.contains("claude"),
        "the engine name is still shown: {label:?}"
    );
    assert!(
        label.contains("model unknown"),
        "the marker is the explicit 'model unknown' wording, not silence: {label:?}"
    );

    // The known case contrasts: the model id is shown, the marker is not.
    let known = attempt("claude", Some("opus"));
    let label = attempt_label(&known);
    assert!(
        label.contains("claude") && label.contains("opus"),
        "a known model renders engine + model: {label:?}"
    );
    assert!(
        !label.contains("model unknown"),
        "the marker never appears when the model is known: {label:?}"
    );
}

// ---------------------------------------------------------------------------
// AC4 — crash recovery keeps both executions' provenance.
// ---------------------------------------------------------------------------

#[test]
fn ac4_steps_reexecuted_after_crash_recovery_keep_provenance_for_both_executions() {
    const ID: &str = "CXA-B259";
    let mut s = ProjectState {
        tickets: vec![Ticket::new(
            tid(ID),
            TicketType::Bug,
            format!("bug {ID}"),
            "fixture",
            Priority::High,
            Complexity::Medium,
            false,
        )
        .expect("ticket")],
        ..ProjectState::default()
    };

    // Pre-crash: a runner claims the bug and its DEV step is recorded.
    s.tickets[0]
        .transition_to(Role::DevBug, Status::InProgress)
        .expect("claim");
    s.record_step_provenance(
        ID,
        step(
            "2026-08-30T12:00:00Z",
            "DEV-BUG",
            "started implementing",
            vec![attempt(
                "opencode",
                Some("bizbrain/Qwen3.6-35B-A3B-thinking"),
            )],
        ),
    );

    // CRASH: the runner dies mid-step — nothing more is written. The
    // pre-crash record is already persisted (the state store save that
    // preceded the crash is the durability boundary).

    // Recovery: `Ticket::release_claim(Role::System)` returns the claim a
    // crashed run left `InProgress` back to the work queue, the restarted
    // runner re-claims, and the re-execution records ITS provenance too.
    s.tickets[0]
        .release_claim(Role::System)
        .expect("crash recovery releases the orphaned claim");
    assert_eq!(
        s.tickets[0].status(),
        Status::Open,
        "the crashed claim returns to the queue"
    );
    s.tickets[0]
        .transition_to(Role::DevBug, Status::InProgress)
        .expect("re-claim after recovery");
    s.record_step_provenance(
        ID,
        step(
            "2026-08-30T12:05:00Z",
            "DEV-BUG",
            "started implementing",
            vec![attempt("claude", Some("opus"))],
        ),
    );
    s.tickets[0]
        .transition_to(Role::DevBug, Status::Fixed)
        .expect("fix after recovery");

    let steps = s.step_provenance(ID);
    assert_eq!(
        steps.len(),
        2,
        "BOTH executions keep their provenance, pre-crash and post-crash: {steps:?}"
    );
    assert_eq!(
        steps[0].attempts[0].engine, "opencode",
        "the pre-crash execution's engine survives: {steps:?}"
    );
    assert_eq!(
        steps[1].attempts[0].engine, "claude",
        "the post-crash execution's engine is recorded alongside it: {steps:?}"
    );
    assert!(
        steps[0].at < steps[1].at,
        "the executions are ordered: the pre-crash run first, the re-execution after"
    );
}

/// The crash boundary in this codebase IS the persisted state: whatever the
/// verifier reads must survive a save/load round-trip unchanged, and state
/// written before the field existed must still load (the `ticket_failures`
/// additive serde-default precedent).
#[test]
fn guard_step_provenance_round_trips_the_store_and_legacy_snapshots_load_clean() {
    let mut s = ProjectState::default();
    s.record_step_provenance(
        "CXA-B260",
        step(
            "2026-08-30T13:00:00Z",
            "DEV-BUG",
            "started implementing",
            vec![attempt("claude", Some("opus"))],
        ),
    );
    let doc = serde_json::to_value(&s).expect("serialize persisted state");
    let back: ProjectState = serde_json::from_value(doc).expect("load persisted state");
    assert_eq!(
        back.step_provenance("CXA-B260"),
        s.step_provenance("CXA-B260"),
        "provenance survives the save/load boundary a crash crosses"
    );

    // A snapshot written before the field existed loads to an empty map —
    // no migration, no stranded project.
    let mut doc = serde_json::to_value(ProjectState::default()).expect("serialize");
    doc.as_object_mut()
        .expect("object")
        .remove("ticket_step_provenance");
    let legacy: ProjectState = serde_json::from_value(doc).expect("load legacy state");
    assert!(
        legacy.step_provenance("CXA-B260").is_empty(),
        "pre-change snapshots load with no provenance, never a guess"
    );
}

// ---------------------------------------------------------------------------
// AC5 — send-back cycles: provenance per attempt, per version.
// ---------------------------------------------------------------------------

#[test]
fn ac5_send_back_cycles_record_provenance_per_attempt_so_each_version_names_its_model() {
    const ID: &str = "CXA-B261";
    let mut s = ProjectState {
        tickets: vec![bug_at_fixed(ID)],
        ..ProjectState::default()
    };
    // Version 1 of the work, produced by the first fix run.
    s.record_step_provenance(
        ID,
        step(
            "2026-08-30T14:00:00Z",
            "DEV-BUG",
            "started implementing",
            vec![attempt("claude", Some("opus"))],
        ),
    );

    // The reviewer sends it back at the verify gate — the exact records
    // inbox.rs writes.
    send_back(
        &mut s,
        ID,
        "2026-08-30T14:20:00Z",
        "rev",
        "the fix is not demonstrated",
    );

    // The dev re-fixes (the legal edges back to Fixed) and version 2 is
    // produced by a DIFFERENT engine/model — its own attempt, its own
    // provenance.
    s.tickets[0]
        .transition_to(Role::DevBug, Status::InProgress)
        .expect("re-claim");
    s.record_step_provenance(
        ID,
        step(
            "2026-08-30T14:40:00Z",
            "DEV-BUG",
            "started implementing",
            vec![attempt(
                "opencode",
                Some("bizbrain/Qwen3.6-35B-A3B-thinking"),
            )],
        ),
    );
    s.tickets[0]
        .transition_to(Role::DevBug, Status::Fixed)
        .expect("re-fix");

    let steps = s.step_provenance(ID);
    assert_eq!(
        steps.len(),
        2,
        "each cycle's attempt keeps its own provenance: {steps:?}"
    );
    assert!(
        steps[0].at < steps[1].at,
        "chronological: version 1 before the send-back, version 2 after"
    );
    assert_eq!(
        steps[0].attempts[0].engine, "claude",
        "version 1's engine is attributed: {steps:?}"
    );
    assert_eq!(
        steps[0].attempts[0].model.as_deref(),
        Some("opus"),
        "version 1's model is attributed: {steps:?}"
    );
    assert_eq!(
        steps[1].attempts[0].engine, "opencode",
        "version 2's engine is attributed separately: {steps:?}"
    );
    assert_eq!(
        steps[1].attempts[0].model.as_deref(),
        Some("bizbrain/Qwen3.6-35B-A3B-thinking"),
        "version 2's model is attributed separately: {steps:?}"
    );
    // The send-back the reviewer authored sits in the same state, so the
    // reviewer can line each version up with the cycle that produced it.
    assert_eq!(
        s.governance_interventions.len(),
        1,
        "the send-back cycle is recorded beside the provenance"
    );
    assert_eq!(
        s.governance_interventions[0].kind,
        InterventionKind::VerifySendBack
    );
}
