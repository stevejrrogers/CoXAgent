//! TDD tests for CXA-F238 — Self-tuning brake cockpit with operator
//! hold/override and audit.
//!
//! These tests encode the ticket's acceptance criteria over the seams that
//! exist today and FAIL (or, where a criterion is a preservation guarantee
//! that trivially holds pre-implementation, PIN the baseline the
//! implementation must not disturb). Fixtures are built only from types the
//! codebase has: [`ProjectState`], [`DeployRecord`] history,
//! `ticket_fail_attempts`, and the pure pipeline the daily pass itself runs —
//! `metrics::agent_evals` → `metrics::compute_burndown` →
//! `metrics::decide_tuning` — driven through [`RunCycleUseCase::run_cycle`],
//! the leader pass whose `self_tune` ceremony IS "the next daily tuning pass".
//!
//! The SA design has since landed (CXA-F238): overrides persist as
//! `ProjectState::tuning_overrides` (`BTreeMap<String, BrakeHold>`, one hold
//! per brake) with an append-only `ProjectState::tuning_history`
//! (`TuningAuditEntry`, cap [`MAX_TUNING_HISTORY`]); holds compose ABOVE
//! `decide_tuning` via `metrics_brakes::apply_brake_holds` so the hysteresis
//! math stays byte-for-byte intact; the operator surfaces are
//! `/api/projects/:pid/brakes` (GET) and `/brakes/:brake/hold`
//! (POST/DELETE) with P5a-style in-handler role enforcement. The
//! override-aware behaviour itself is gated in
//! `crates/app/tests/tunecockpit_gate.rs`; this file keeps the pre-override
//! baseline pins the ACs require to hold ("re-applies autonomous decisions
//! UNCHANGED", "bit-for-bit identical") plus the legacy-load contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use coxagent_application::config::{Config, GitConfig};
use coxagent_application::metrics::BURNDOWN_WINDOW_DAYS;
use coxagent_application::metrics::{agent_evals, compute_burndown, decide_tuning};
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, GitAuthor, GitPort, SandboxStatus, StateStorePort,
    SyncBase,
};
use coxagent_application::state::{now_rfc3339, DeployRecord, ProjectState, Tuning};
use coxagent_application::use_cases::RunCycleUseCase;
use coxagent_application::PortError;
use coxagent_domain::{Complexity, Priority, SemVer, Status, Ticket, TicketId, TicketType};
use std::path::Path;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Doubles — pure, in-memory, no IO beyond what the ports define. The same
// world `revert_learning_f047_tdd.rs` proved reaches the leader ceremonies
// (including `self_tune`) with zero network and zero engine calls.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MemStore {
    state: Mutex<ProjectState>,
}

#[async_trait]
impl StateStorePort for MemStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().unwrap().clone())
    }
    async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
        s.validate().map_err(PortError::Corrupt)?;
        *self.state.lock().unwrap() = s.clone();
        Ok(())
    }
}

/// A git adapter that answers "git did nothing" — the port's own convention
/// for doubles. The tuning pass reads no git; the double only keeps the
/// cycle's git-touching phases inert.
struct NoGit;

#[async_trait]
impl GitPort for NoGit {
    async fn raw(&self, _work_dir: &Path, _args: &[&str]) -> (bool, String) {
        (false, String::new())
    }
    async fn is_repo(&self, _: &Path) -> bool {
        true
    }
    async fn current_branch(&self, _: &Path) -> Result<String, PortError> {
        Ok("main".to_owned())
    }
    async fn checkout_branch(&self, _: &Path, _: &str) -> Result<(), PortError> {
        Ok(())
    }
    async fn commit_all(
        &self,
        _: &Path,
        _message: &str,
        _author: &GitAuthor,
    ) -> Result<Option<String>, PortError> {
        Ok(None)
    }
    async fn push(&self, _: &Path, _: &str) -> Result<(), PortError> {
        Ok(())
    }
    async fn sync_base(&self, _: &Path, _: &str) -> Result<SyncBase, PortError> {
        Ok(SyncBase::UpToDate)
    }
    async fn abort_merge(&self, _: &Path) -> Result<(), PortError> {
        Ok(())
    }
}

/// An engine that proposes nothing and reports nothing — the cycle's agent
/// phases are no-ops, so the only state change under test is the daily
/// tuning pass.
struct Silent;

#[async_trait]
impl AgentEnginePort for Silent {
    fn id(&self) -> &'static str {
        "silent"
    }
    async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
        Ok(AgentOutcome {
            stdout: "[]".to_owned(),
            stderr: String::new(),
            exit_code: Some(0),
            usage: None,
            trace: String::new(),
            session_id: None,
            sandbox: SandboxStatus::default(),
            engine: String::new(),
            model: String::new(),
            attempts: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// Fixtures — every field from types the codebase actually has.
// ---------------------------------------------------------------------------

/// A feature ticket placed directly in `status` via serde — the same fixture
/// style as `metrics::tests` in the application crate (placement through
/// serde is legal for DONE: `validate()` checks ids and dependencies, not
/// transition history).
fn feature_in_status(id: &str, status: Status) -> Ticket {
    let status_key = |s: Status| -> &'static str {
        match s {
            Status::Pending => "pending",
            Status::Ready => "ready",
            Status::InProgress => "in_progress",
            Status::Done => "done",
            Status::Documented => "documented",
            Status::Rejected => "rejected",
            Status::Open => "open",
            Status::Fixed => "fixed",
            Status::Verified => "verified",
            Status::OnHold => "on_hold",
        }
    };
    let json = serde_json::json!({
        "id": id, "type": "feature", "title": "t", "description": "",
        "priority": "medium", "complexity": "small", "status": status_key(status),
        "has_ui": false, "design": {"technical": null, "ux": null},
        "parent_id": null, "depends_on": []
    });
    serde_json::from_value(json).expect("ticket")
}

/// A REAL lifecycle feature still awaiting refinement — backlog fodder the
/// self-tune backlog count sees (`Pending`), built through the aggregate's
/// own constructor so every invariant holds by construction. Titles are
/// semantically unrelated: the cycle's dedup ceremony rejects near-duplicates
/// (Jaccard ≥ 0.6 on title tokens) and this criterion pins the pass as a
/// tuning-only mutation.
fn pending_feature(id: &str, title: &str) -> Ticket {
    Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Feature,
        title.to_owned(),
        format!("the pending feature {id}"),
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket")
}

/// A team whose evals sit DECIDEDLY inside the quality brake's hot band:
/// three ships in the last 7 days, six failed attempts (churn 2.0 > 1.5),
/// a small backlog (2 pending), an empty bug burn-down (delta 0 — no
/// escalation noise). The autonomous decision is therefore non-trivial:
/// bugs_first flips ON from OFF, skip_ba stays OFF.
fn shipped_team_state() -> ProjectState {
    let mut s = ProjectState {
        current_version: SemVer::new(1, 0, 0),
        ..ProjectState::default()
    };
    for n in 1..=3 {
        let id = format!("CXA-F10{n}");
        s.tickets.push(feature_in_status(&id, Status::Done));
        s.history.push(DeployRecord {
            version: SemVer::new(1, 0, 0),
            ticket: TicketId::new(&id).expect("id"),
            title: "t".to_owned(),
            at: now_rfc3339(),
        });
    }
    s.tickets.push(pending_feature(
        "CXA-F201",
        "Migrate billing exports to a nightly batch job",
    ));
    s.tickets.push(pending_feature(
        "CXA-F202",
        "Add keyboard shortcuts to the search palette",
    ));
    // Retry churn: six failed attempts across two tickets — churn_per_ship
    // 6/3 = 2.0, hot enough to trip the quality brake's 1.5 threshold.
    s.ticket_fail_attempts.insert("CXA-F201".to_owned(), 3);
    s.ticket_fail_attempts.insert("CXA-F202".to_owned(), 3);
    s
}

fn config() -> Config {
    Config {
        git: GitConfig {
            enabled: true,
            ..GitConfig::default()
        },
        ..Config::default()
    }
}

/// The autonomous decision, computed EXACTLY the way the daily pass computes
/// it (`ceremonies::self_tune`): evals from state, backlog as the
/// pending/ready/open count, burn-down delta over the standard window. Pure.
fn autonomous_decision(pre: &ProjectState) -> Tuning {
    let evals = agent_evals(pre);
    let backlog = pre
        .tickets
        .iter()
        .filter(|t| matches!(t.status(), Status::Pending | Status::Ready | Status::Open))
        .count();
    let today = now_rfc3339()[..10].to_owned();
    let delta = compute_burndown(pre, &today, BURNDOWN_WINDOW_DAYS).delta_24h;
    decide_tuning(&evals, backlog, delta, &pre.tuning)
}

/// Boot the cycle world over `state` — boxed: the cycle future is huge.
fn world(state: ProjectState) -> (Arc<MemStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    (store, dir)
}

/// One leader pass — the run whose daily ceremonies include the self-tuning
/// pass. Boxed: the cycle future is huge.
async fn run_one_cycle(store: &Arc<MemStore>, dir: &tempfile::TempDir) {
    let uc = RunCycleUseCase::new(
        Arc::clone(store),
        Arc::new(Silent),
        config(),
        dir.path().to_path_buf(),
        "goal".to_owned(),
    )
    .with_git(Arc::new(NoGit) as Arc<dyn GitPort>);
    Box::pin(uc.run_cycle(1)).await;
}

// ---------------------------------------------------------------------------
// AC2 (tail) — "…after expiry the next daily tuning pass re-applies
// autonomous decisions unchanged."
//
// The override mechanism that "expiry" belongs to does not exist yet (see
// the ASK SA block), so the testable core of this criterion is the
// INVARIANT the pass must keep satisfying once overrides exist: the pass
// persists EXACTLY the autonomous decision over real project-state data —
// nothing more, nothing less — and touches nothing else. This is the same
// derivation guarantee AC1 demands ("computed purely by applying
// decide_tuning() to real project-state data"); the cockpit's
// signals+thresholds PRESENTATION half is a design gap and is not faked.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac2_tail_the_daily_pass_reapplies_the_autonomous_decision_exactly_unchanged() {
    let pre_state = shipped_team_state();
    let expected_base = autonomous_decision(&pre_state);
    let (history_before, tickets_before, version_before) = (
        pre_state.history.clone(),
        pre_state.tickets.clone(),
        pre_state.current_version.clone(),
    );
    let (store, dir) = world(pre_state);
    run_one_cycle(&store, &dir).await;

    let after = store.load().await.unwrap();
    let mut expected = expected_base;
    // The pass stamps the eval day on the decision it persisted.
    expected.last_eval_day = now_rfc3339()[..10].to_owned();
    assert_eq!(
        after.tuning, expected,
        "the persisted tuning must be EXACTLY the autonomous decision over real state"
    );
    // Non-vacuous: the fixture's hot churn (2.0 > 1.5) must actually have
    // flipped the quality brake — the assertion above must be able to fail.
    assert!(
        after.tuning.bugs_first,
        "hot churn tripped the quality brake (churn 2.0 > 1.5)"
    );
    assert!(
        !after.tuning.skip_ba,
        "a shipping team keeps the intake brake off"
    );
    // "…unchanged": the pass is a tuning-only mutation — no ticket, deploy
    // or version moves as a side effect.
    assert_eq!(after.history, history_before, "deploy history untouched");
    assert_eq!(after.tickets, tickets_before, "tickets untouched");
    assert_eq!(after.current_version, version_before, "version untouched");
}

// ---------------------------------------------------------------------------
// AC5 (edge case 2) — "projects whose persisted state predates this field
// load without migration failure (missing key defaults), show no phantom
// overrides, and cycle behaviour for untouched projects is bit-for-bit
// identical."
//
// "This field" is the override field the SA has not named yet; a document
// persisted TODAY is by construction a pre-field document, so these tests
// pin the load/phantom/identity contract against today's exact serialized
// shape and keep holding once the field lands behind `#[serde(default)]`
// (the pattern every prior state addition here used — `burn_mode`,
// `bug_burn_floor`, `governance_interventions`).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac5_a_state_document_predating_the_field_loads_without_migration_failure() {
    // An engaged tuning (every field set) round-trips byte-identically
    // through a document that predates the override field.
    let mut s = shipped_team_state();
    s.tuning = Tuning {
        bugs_first: true,
        skip_ba: true,
        burn_mode: true,
        burn_until_bugs_le: Some(2),
        last_eval_day: "2026-01-01".to_owned(),
    };
    let doc = serde_json::to_value(&s).expect("serialize state");
    let back: ProjectState = serde_json::from_value(doc).expect("load legacy state");
    assert_eq!(
        back.tuning, s.tuning,
        "an old-store document loads with its tuning intact — no migration, no loss"
    );

    // The literal "missing key defaults" clause: a document with the whole
    // tuning object absent loads with the serde defaults, like every field
    // that predates its own reader. (A default tuning serializes to NO key
    // at all — `skip_serializing_if = "Tuning::is_default"` — so strip it
    // from an ENGAGED document, which is the shape an old writer could see.)
    let mut engaged = shipped_team_state();
    engaged.tuning = Tuning {
        bugs_first: true,
        burn_mode: true,
        last_eval_day: "2026-01-01".to_owned(),
        ..Tuning::default()
    };
    let mut stripped = serde_json::to_value(engaged).expect("serialize");
    stripped
        .as_object_mut()
        .expect("object")
        .remove("tuning")
        .expect("tuning key was present on the engaged document");
    let back: ProjectState = serde_json::from_value(stripped).expect("load tuning-less state");
    assert_eq!(back.tuning, Tuning::default());

    // Same clause for the override collections: a document that carries them
    // (non-empty, so they serialize) loads cleanly once a REST-fronted
    // runner or older hub strips the keys it does not know.
    let mut engaged = shipped_team_state();
    engaged.tuning_overrides.insert(
        "bugs_first".to_owned(),
        coxagent_application::state::BrakeHold {
            pinned_value: Some(true),
            reason: "hold".to_owned(),
            actor: "operator".to_owned(),
            at: "2026-08-01T00:00:00Z".to_owned(),
            expires_at: "2026-08-02T00:00:00Z".to_owned(),
        },
    );
    engaged
        .tuning_history
        .push(coxagent_application::state::TuningAuditEntry {
            at: "2026-08-01T00:00:00Z".to_owned(),
            actor: "SM".to_owned(),
            source: "hold".to_owned(),
            brake: "bugs_first".to_owned(),
            from: false,
            to: true,
            reason: "hold".to_owned(),
            until: Some("2026-08-02T00:00:00Z".to_owned()),
        });
    let mut doc = serde_json::to_value(&engaged).expect("serialize state");
    let obj = doc.as_object_mut().expect("object");
    obj.remove("tuning_overrides")
        .expect("overrides key present");
    obj.remove("tuning_history").expect("history key present");
    let back: ProjectState = serde_json::from_value(doc).expect("load stripped state");
    assert!(
        back.tuning_overrides.is_empty() && back.tuning_history.is_empty(),
        "missing keys default to empty — no migration failure, no phantom overrides"
    );
}

#[tokio::test]
async fn ac5_loaded_legacy_state_shows_no_phantom_overrides() {
    let mut s = shipped_team_state();
    s.tuning = Tuning {
        bugs_first: true,
        ..Tuning::default()
    };
    let doc = serde_json::to_value(&s).expect("serialize state");
    let obj = doc.as_object().expect("state object");

    // The tuning object carries exactly today's known keys — no
    // override-shaped residue serializes into documents old readers see.
    let tuning_keys: std::collections::BTreeSet<String> = obj
        .get("tuning")
        .expect("engaged tuning serializes")
        .as_object()
        .expect("tuning object")
        .keys()
        .cloned()
        .collect();
    let expected_keys: std::collections::BTreeSet<String> = [
        // burn_until_bugs_le is `skip_serializing_if = Option::is_none` and the
        // fixture leaves it None — a sparse tuning serializes exactly these.
        "bugs_first",
        "skip_ba",
        "burn_mode",
        "last_eval_day",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        tuning_keys, expected_keys,
        "no phantom override key may appear in the persisted tuning object"
    );

    // The SA named the override collections (tuning_overrides/tuning_history);
    // while EMPTY they serialize to NOTHING (skip_serializing_if), so
    // documents untouched projects write stay byte-shaped like before.
    assert!(
        obj.get("tuning_overrides").is_none(),
        "an untouched project must not gain a tuning_overrides key"
    );
    assert!(
        obj.get("tuning_history").is_none(),
        "an untouched project must not gain a tuning_history key"
    );
    let back: ProjectState = serde_json::from_value(doc).expect("reload");
    assert!(
        back.tuning_overrides.is_empty() && back.tuning_history.is_empty(),
        "and loads with no phantom overrides"
    );
}

#[tokio::test]
async fn ac5_cycle_behaviour_for_untouched_projects_is_bit_for_bit_identical() {
    // Two identical worlds: one in-memory, one persisted and reloaded —
    // i.e. a project whose store predates the override field. One cycle
    // each; the daily pass must land the EXACT same tuning in both.
    let fresh_state = shipped_team_state();
    let expected_base = autonomous_decision(&fresh_state);

    let doc = serde_json::to_value(&fresh_state).expect("serialize state");
    let reloaded: ProjectState = serde_json::from_value(doc).expect("load legacy state");
    assert_eq!(reloaded.tuning, fresh_state.tuning);

    let (store_a, dir_a) = world(fresh_state);
    let (store_b, dir_b) = world(reloaded);
    run_one_cycle(&store_a, &dir_a).await;
    run_one_cycle(&store_b, &dir_b).await;

    let a = store_a.load().await.unwrap();
    let b = store_b.load().await.unwrap();
    assert_eq!(
        a.tuning, b.tuning,
        "the reloaded (pre-field) project's daily pass lands the identical tuning"
    );
    let mut expected = expected_base;
    expected.last_eval_day = now_rfc3339()[..10].to_owned();
    assert_eq!(
        a.tuning, expected,
        "and identical to the autonomous decision"
    );
    assert!(
        a.tuning.bugs_first,
        "non-vacuous: the brake actually flipped in both worlds"
    );
}

// ---------------------------------------------------------------------------
// NOT TESTED HERE — the override-aware behaviour lives where the SA design
// pinned it, `crates/app/tests/tunecockpit_gate.rs` (set/clear composition,
// expiry filtering ahead of the cycle's reads, the audit trail's shape and
// cap, the cockpit payload, and the bit-exact SM announcement wording).
// The AC1 "dashboard shows" payload is served by GET /api/projects/:pid/brakes
// (crates/presentation/src/server/tunecockpit.rs) and covered end to end by
// e2e/specs-auth/brakes.spec.ts.
// ---------------------------------------------------------------------------
