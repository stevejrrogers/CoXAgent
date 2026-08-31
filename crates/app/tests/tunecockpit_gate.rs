//! CXA-F238 acceptance gate — the self-tuning brake cockpit with operator
//! hold/override and audit.
//!
//! Pure unit tests over struct-literal fixtures built from the real types
//! (`ProjectState`, `Tuning`, `BrakeHold`, `TuningAuditEntry`) plus the exact
//! pipeline the daily pass runs (`agent_evals` → `compute_burndown` →
//! `decide_tuning`) — the same MemStore harness precedent
//! `burndown_f022_gate.rs` established: no server, no host harness, no network
//! port. The two cycle-level tests boot the leader pass through
//! `RunCycleUseCase` because their criteria are about WHEN the filtering
//! happens relative to the cycle's own reads.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use coxagent_application::config::{Config, GitConfig};
use coxagent_application::metrics::{
    agent_evals, apply_brake_holds, brake_backlog, brake_cockpit, clear_brake_hold,
    compute_burndown, decide_tuning, reconcile_brake_holds, set_brake_hold, split_expired_holds,
    BACKLOG_BRAKE_OFF, BACKLOG_BRAKE_ON, BURNDOWN_WINDOW_DAYS, CHURN_BRAKE_OFF, CHURN_BRAKE_ON,
};
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, GitAuthor, GitPort, SandboxStatus, StateStorePort,
    SyncBase,
};
use coxagent_application::state::{
    now_rfc3339, BrakeHold, DeployRecord, ProjectState, Tuning, TuningAuditEntry,
    MAX_TUNING_HISTORY,
};
use coxagent_application::use_cases::RunCycleUseCase;
use coxagent_application::PortError;
use coxagent_domain::{Complexity, Priority, SemVer, Status, Ticket, TicketId, TicketType};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Doubles — the same inert world `tuning_brakes_f238_tdd.rs` proved reaches
// the leader ceremonies (including `self_tune`) with zero network and zero
// engine calls.
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

fn pending_feature(id: &str) -> Ticket {
    Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Feature,
        "pending work",
        "the pending feature",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket")
}

/// A team with three ships in the last 7 days, retry churn 2.0 (hot, trips
/// the quality brake's ON band) and a small backlog. The autonomous decision
/// is non-trivial: bugs_first ON, skip_ba OFF.
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
    s.tickets.push(pending_feature("CXA-F201"));
    s.tickets.push(pending_feature("CXA-F202"));
    s.ticket_fail_attempts.insert("CXA-F201".to_owned(), 3);
    s.ticket_fail_attempts.insert("CXA-F202".to_owned(), 3);
    s
}

/// The autonomous decision exactly the way the daily pass computes it. Pure.
fn autonomous_decision(pre: &ProjectState) -> Tuning {
    let evals = agent_evals(pre);
    let backlog = brake_backlog(pre);
    let today = now_rfc3339()[..10].to_owned();
    let delta = compute_burndown(pre, &today, BURNDOWN_WINDOW_DAYS).delta_24h;
    decide_tuning(&evals, backlog, delta, &pre.tuning)
}

fn hold(pinned: Option<bool>, reason: &str, expires_at: &str) -> BrakeHold {
    BrakeHold {
        pinned_value: pinned,
        reason: reason.to_owned(),
        actor: "operator".to_owned(),
        at: "2026-08-01T00:00:00Z".to_owned(),
        expires_at: expires_at.to_owned(),
    }
}

fn future_stamp() -> String {
    let t = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    t.format(&time::format_description::well_known::Rfc3339)
        .expect("rfc3339")
}

fn past_stamp() -> String {
    let t = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    t.format(&time::format_description::well_known::Rfc3339)
        .expect("rfc3339")
}

/// Boot the cycle world over `state` — boxed: the cycle future is huge.
fn world(state: ProjectState) -> (Arc<MemStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    (store, dir)
}

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

fn config() -> Config {
    Config {
        git: GitConfig {
            enabled: true,
            ..GitConfig::default()
        },
        ..Config::default()
    }
}

// ---------------------------------------------------------------------------
// (a) The reconciliation is PURE and rides ABOVE the hysteresis.
// ---------------------------------------------------------------------------

#[test]
fn a_apply_pins_some_value_overrides_raw_even_when_hysteresis_disagrees_and_leaves_unrelated_field_to_normal_policy() {
    // The loop's raw verdict: quality brake ON (hot churn), intake OFF.
    let raw = Tuning {
        bugs_first: true,
        skip_ba: false,
        ..Tuning::default()
    };
    let current = Tuning::default();
    let mut holds = BTreeMap::new();
    // The operator pins the quality brake OFF although the hysteresis says ON.
    holds.insert("bugs_first".to_owned(), hold(Some(false), "not yet", &future_stamp()));
    let effective = apply_brake_holds(&raw, &holds, &current);
    assert!(!effective.bugs_first, "the pin wins over the raw policy");
    assert!(
        !effective.skip_ba,
        "the unheld field keeps the normal (raw) policy untouched"
    );
}

#[test]
fn a_frozen_hold_keeps_the_current_value_not_recomputed_even_if_inputs_would_flip_it() {
    // Inputs would flip skip_ba ON; a freeze (None pin) holds the CURRENT
    // value instead of recomputing.
    let raw = Tuning {
        bugs_first: false,
        skip_ba: true,
        ..Tuning::default()
    };
    let current = Tuning {
        skip_ba: false,
        ..Tuning::default()
    };
    let mut holds = BTreeMap::new();
    holds.insert("skip_ba".to_owned(), hold(None, "mid-migration", &future_stamp()));
    let effective = apply_brake_holds(&raw, &holds, &current);
    assert!(!effective.skip_ba, "frozen at the current value");
    assert!(effective.bugs_first == raw.bugs_first, "unheld field = raw");
}

// ---------------------------------------------------------------------------
// (b) Clearing resumes the normal policy; passes are idempotent.
// ---------------------------------------------------------------------------

#[test]
fn b_clearing_a_hold_resumes_normal_policy_next_pass() {
    let mut s = shipped_team_state();
    // The loop wants skip_ba OFF (team shipping); the operator pins it ON.
    assert!(set_brake_hold(&mut s, "skip_ba", hold(Some(true), "freeze intake", &future_stamp())));
    assert!(s.tuning.skip_ba, "the hold lands immediately");
    assert_eq!(s.tuning_overrides["skip_ba"].pinned_value, Some(true));
    assert!(
        clear_brake_hold(&mut s, "skip_ba", "operator2"),
        "the hold was there to clear"
    );
    assert!(
        s.tuning_overrides.is_empty(),
        "cleared hold leaves no residue"
    );
    assert_eq!(
        s.tuning.skip_ba,
        autonomous_decision(&s).skip_ba,
        "the next pass resumes the autonomous policy"
    );
    // The trail shows both directions of the intervention.
    let sources: Vec<&str> = s.tuning_history.iter().map(|e| e.source.as_str()).collect();
    assert_eq!(sources, vec!["hold", "clear"], "hold then clear, in order");
    let clear = s.tuning_history.last().expect("clear entry");
    assert_eq!(clear.actor, "operator2");
    assert_eq!(clear.brake, "skip_ba");
    assert!(!clear.to, "cleared back to the autonomous value");
}

#[test]
fn b_reconcile_passes_are_idempotent_given_unchanged_inputs() {
    let mut s = shipped_team_state();
    // Active (unexpired) hold: recomposition changes nothing, twice.
    s.tuning_overrides
        .insert("bugs_first".to_owned(), hold(Some(false), "hold", &future_stamp()));
    s.tuning.bugs_first = false;
    let before = s.tuning.clone();
    assert!(!reconcile_brake_holds(&mut s, &now_rfc3339()), "nothing expired");
    assert_eq!(s.tuning, before, "active holds ride, nothing moves");
    // Expired hold: the first pass releases it, the second is a no-op.
    s.tuning_overrides
        .insert("bugs_first".to_owned(), hold(Some(false), "hold", &past_stamp()));
    assert!(reconcile_brake_holds(&mut s, &now_rfc3339()), "expired → released");
    let released = s.tuning.clone();
    assert!(!reconcile_brake_holds(&mut s, &now_rfc3339()), "second pass is a no-op");
    assert_eq!(s.tuning, released, "and nothing moved on the second pass");
}

// ---------------------------------------------------------------------------
// (c) The audit trail: every change, source + field-wise from/to, bounded.
// ---------------------------------------------------------------------------

#[test]
fn c_audit_appends_every_change_with_source_and_fieldwise_from_to() {
    let mut s = shipped_team_state();
    // Autonomous pass shape (what self_tune records): a flip with from/to.
    s.record_tuning_change(TuningAuditEntry {
        at: now_rfc3339(),
        actor: "SM".to_owned(),
        source: "self_tune".to_owned(),
        brake: "bugs_first".to_owned(),
        from: false,
        to: true,
        reason: "quality brake ON — retry churn 2.00/ship; features pause, bugs first".to_owned(),
        until: None,
    });
    // Operator hold on the other brake, with its window.
    let window = future_stamp();
    set_brake_hold(&mut s, "skip_ba", hold(Some(true), "freeze intake", &window));
    let flip = &s.tuning_history[0];
    assert_eq!(flip.source, "self_tune");
    assert_eq!(flip.brake, "bugs_first");
    assert!(!flip.from && flip.to, "field-wise direction recorded");
    assert!(flip.until.is_none(), "autonomous flips carry no window");
    let set = s.tuning_history.last().expect("hold entry");
    assert_eq!(set.source, "hold");
    assert_eq!(set.brake, "skip_ba");
    assert_eq!(set.reason, "freeze intake", "the operator's own reason");
    assert_eq!(set.until.as_deref(), Some(window.as_str()), "window recorded");
}

#[test]
fn c_freeze_hold_that_changes_nothing_is_still_audited() {
    // A freeze (None pin) on a brake whose value already matches what the
    // operator wants changes no state value — but the INTERVENTION itself is
    // a governance event and lands in the trail with its window.
    let mut s = shipped_team_state();
    assert!(!s.tuning.skip_ba, "fixture: intake brake already off");
    assert!(set_brake_hold(&mut s, "skip_ba", hold(None, "hold it off", &future_stamp())));
    let entry = s.tuning_history.last().expect("hold entry");
    assert_eq!(entry.source, "hold");
    assert_eq!(entry.from, entry.to, "value untouched, event still recorded");
    assert_eq!(entry.reason, "hold it off");
}

#[test]
fn c_history_cap_prunes_oldest_at_exactly_500_non_fatal() {
    const TOTAL: usize = MAX_TUNING_HISTORY + 2;
    let mut s = ProjectState::default();
    for n in 0..TOTAL {
        s.record_tuning_change(TuningAuditEntry {
            at: format!("2026-01-01T00:00:{n:02}Z"),
            actor: "SM".to_owned(),
            source: "self_tune".to_owned(),
            brake: "bugs_first".to_owned(),
            from: false,
            to: true,
            reason: format!("entry {n}"),
            until: None,
        });
    }
    assert_eq!(s.tuning_history.len(), MAX_TUNING_HISTORY, "bounded, not fatal");
    assert_eq!(
        s.tuning_history[0].reason,
        format!("entry {}", TOTAL - MAX_TUNING_HISTORY),
        "the OLDEST entries were the ones dropped"
    );
    assert_eq!(
        s.tuning_history.last().expect("newest kept").reason,
        format!("entry {}", TOTAL - 1),
    );
}

// ---------------------------------------------------------------------------
// (d) The SM announcement wording for pure auto flips is bit-exact — the
// cockpit must not have moved the existing voice.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn d_announcement_strings_match_existing_sm_wording_for_pure_auto_flips() {
    let (store, dir) = world(shipped_team_state());
    run_one_cycle(&store, &dir).await;
    let after = store.load().await.unwrap();
    let tuning_msgs: Vec<&str> = after
        .comments
        .iter()
        .filter(|c| c.author == "SM" && c.body.starts_with("🎛️ Self-tuning:"))
        .map(|c| c.body.as_str())
        .collect();
    assert_eq!(
        tuning_msgs,
        vec![
            "🎛️ Self-tuning: quality brake ON — retry churn 2.00/ship; features pause, bugs first",
        ],
        "bit-exact existing SM wording (churn 6/3 = 2.00)"
    );
}

// ---------------------------------------------------------------------------
// (d2) AC2's core: an ACTIVE hold survives the daily pass and pins the brake
// AGAINST what the autonomous verdict wants — this is the whole point of the
// cockpit, so it is tested at the cycle level, not only on the pure fn.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn d2_active_hold_survives_the_daily_pass_and_pins_against_the_loop() {
    let mut pre = shipped_team_state();
    let auto = autonomous_decision(&pre);
    assert!(auto.bugs_first, "fixture: hot churn wants the brake ON");
    // The operator disagrees and pins it OFF with a still-future bound.
    pre.tuning_overrides.insert(
        "bugs_first".to_owned(),
        hold(Some(false), "release gate is mid-flight", &future_stamp()),
    );
    pre.tuning.bugs_first = false;
    let (store, dir) = world(pre);
    run_one_cycle(&store, &dir).await;

    let after = store.load().await.unwrap();
    assert!(
        !after.tuning.bugs_first,
        "the pin rode above the hysteresis: the brake did NOT flip"
    );
    assert!(
        after
            .tuning_overrides
            .get("bugs_first")
            .is_some_and(|h| h.pinned_value == Some(false)),
        "the hold persists across the daily re-tune"
    );
    assert!(
        !after
            .tuning_history
            .iter()
            .any(|e| e.source == "self_tune" && e.brake == "bugs_first"),
        "no autonomous flip entry: the effective value never moved"
    );
    assert!(
        !after
            .comments
            .iter()
            .any(|c| c.body.contains("quality brake ON")),
        "the SM does not announce a flip the operator is overriding"
    );
}

// ---------------------------------------------------------------------------
// Threshold drift guard: the published constants ARE decide_tuning's bars.
// ---------------------------------------------------------------------------

#[test]
fn thresholds_published_as_data_match_decide_tuning_behavior() {
    let evals_for = |churn: f64| coxagent_application::metrics::AgentEvals {
        per_role: vec![],
        shipped_total: 10,
        shipped_7d: 0,
        parked: 0,
        failed_attempts: 0,
        churn_per_ship: churn,
        prs_stuck: 0,
        cost_per_ship_usd: 0.0,
    };
    let off = Tuning::default();
    // Strictly above CHURN_BRAKE_ON trips; AT the bar does not (hysteresis).
    assert!(decide_tuning(&evals_for(CHURN_BRAKE_ON + 0.01), 0, 0, &off).bugs_first);
    assert!(!decide_tuning(&evals_for(CHURN_BRAKE_ON), 0, 0, &off).bugs_first);
    // Strictly below CHURN_BRAKE_OFF releases an engaged brake; AT it does not.
    let on = Tuning {
        bugs_first: true,
        ..Tuning::default()
    };
    assert!(!decide_tuning(&evals_for(CHURN_BRAKE_OFF - 0.01), 0, 0, &on).bugs_first);
    assert!(decide_tuning(&evals_for(CHURN_BRAKE_OFF), 0, 0, &on).bugs_first);
    // Backlog bands (stalled week): > BACKLOG_BRAKE_ON trips, < BACKLOG_BRAKE_OFF releases.
    assert!(decide_tuning(&evals_for(0.0), BACKLOG_BRAKE_ON + 1, 0, &off).skip_ba);
    assert!(!decide_tuning(&evals_for(0.0), BACKLOG_BRAKE_ON, 0, &off).skip_ba);
    let intake_on = Tuning {
        skip_ba: true,
        ..Tuning::default()
    };
    assert!(!decide_tuning(&evals_for(0.0), BACKLOG_BRAKE_OFF - 1, 0, &intake_on).skip_ba);
    assert!(decide_tuning(&evals_for(0.0), BACKLOG_BRAKE_OFF, 0, &intake_on).skip_ba);
}

// ---------------------------------------------------------------------------
// AC4 edge case: an expired-but-not-yet-re-evaluated hold is filtered BEFORE
// the cycle reads skip_ba/bugs_first — the forced value never survives its
// bound, even mid-day between daily passes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expired_hold_is_filtered_before_the_cycle_reads_the_brakes() {
    let mut pre = shipped_team_state();
    let auto = autonomous_decision(&pre);
    assert!(auto.bugs_first, "fixture: hot churn wants the brake ON");
    // An operator released it anyway; that hold has EXPIRED and the persisted
    // tuning still carries the forced OFF (the daily pass has not re-run).
    pre.tuning_overrides
        .insert("bugs_first".to_owned(), hold(Some(false), "ship anyway", &past_stamp()));
    pre.tuning.bugs_first = false;

    let (store, dir) = world(pre);
    run_one_cycle(&store, &dir).await;
    let after = store.load().await.unwrap();

    assert!(
        after.tuning_overrides.is_empty(),
        "the expired hold is gone — no stale force persists past its bound"
    );
    let mut expected = auto;
    expected.last_eval_day = now_rfc3339()[..10].to_owned();
    assert_eq!(
        after.tuning, expected,
        "the cycle reads the autonomous value, not the expired force"
    );
    // And the release is audited.
    assert!(
        after
            .tuning_history
            .iter()
            .any(|e| e.source == "expiry" && e.brake == "bugs_first" && !e.from && e.to),
        "the expiry release lands in the audit trail"
    );
}

// ---------------------------------------------------------------------------
// AC1/AC3 read model: signals + thresholds + active holds + viewer-readable
// trail, derived from ONE source collection.
// ---------------------------------------------------------------------------

#[test]
fn cockpit_reports_signals_thresholds_holds_and_trail() {
    let mut s = shipped_team_state();
    s.tuning = autonomous_decision(&s);
    s.tuning_overrides
        .insert("bugs_first".to_owned(), hold(Some(false), "not yet", &future_stamp()));
    s.tuning.bugs_first = false;
    let now = now_rfc3339();
    let c = brake_cockpit(&s, &now);
    let quality = c
        .cards
        .iter()
        .find(|card| card.id == "quality")
        .expect("quality card");
    assert_eq!(quality.field_name, "bugs_first");
    assert_eq!(quality.mode, "overridden");
    assert!(!quality.effective_value, "the pinned value is what consumers read");
    assert!(quality.auto_would_be, "and the loop would want ON");
    let intake = c.cards.iter().find(|card| card.id == "intake").expect("intake card");
    assert_eq!(intake.mode, "auto", "the unheld brake is autonomous");
    assert_eq!(c.inputs.backlog, brake_backlog(&s));
    assert_eq!(c.inputs.shipped_last7_days, agent_evals(&s).shipped_7d);
    assert!((c.thresholds.churn_on - CHURN_BRAKE_ON).abs() < f64::EPSILON);
    assert_eq!(c.active_holds.len(), 1);
    assert_eq!(c.active_holds[0].brake, "bugs_first");
    assert_eq!(c.active_holds[0].pinned_value, Some(false));
    assert_eq!(c.active_holds[0].reason, "not yet");
    // An expired hold is not an active hold.
    let mut s2 = shipped_team_state();
    s2.tuning_overrides
        .insert("skip_ba".to_owned(), hold(None, "old", &past_stamp()));
    assert!(
        brake_cockpit(&s2, &now).active_holds.is_empty(),
        "expired holds never show as active"
    );
}

#[test]
fn unknown_brake_fields_are_rejected_by_the_pure_layer() {
    let mut s = ProjectState::default();
    assert!(!set_brake_hold(&mut s, "burn_mode", hold(Some(true), "x", &future_stamp())));
    assert!(!clear_brake_hold(&mut s, "burn_mode", "op"));
    assert!(s.tuning_overrides.is_empty());
}

#[test]
fn split_expired_holds_fails_closed_on_unparsable_bounds() {
    let mut holds = BTreeMap::new();
    holds.insert("bugs_first".to_owned(), hold(Some(true), "x", "not-a-time"));
    let (active, expired) = split_expired_holds(&holds, &now_rfc3339());
    assert!(active.is_empty() && expired.len() == 1);
}
