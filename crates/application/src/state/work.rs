// Part of the `state` module split by bounded context — see state/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Work types: sprints, milestones, failures, spend and self-tuning.

use serde::{Deserialize, Serialize};

use super::*;

/// Accumulated engine spend — the FinOps view of the autonomous team.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub total_cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub runs: u64,
    /// Cost attributed per agent role (e.g. `dev_feature`).
    #[serde(default)]
    pub by_role: std::collections::BTreeMap<String, f64>,
    /// Engine runs per role — divides `by_role` into an average cost per run,
    /// the basis of the pre-claim cost estimate for the approval gate.
    #[serde(default)]
    pub runs_by_role: std::collections::BTreeMap<String, u64>,
    /// Cost metered over the SAME window as `runs_by_role` (both started
    /// together) — `by_role` holds all-time totals from before run counting
    /// existed, so dividing THAT by runs inflates the estimate wildly.
    #[serde(default)]
    pub metered_cost_by_role: std::collections::BTreeMap<String, f64>,
    /// Usage attributed per operator (`account@host`) — the SaaS per-user view,
    /// so each user's token spend is measurable even though they share a project.
    #[serde(default)]
    pub by_operator: std::collections::BTreeMap<String, OperatorSpend>,
    /// The engine CLI each role most recently RAN ON (`copilot`, `opencode`, …),
    /// last-wins. This is the engine actually observed (post-failover), not the
    /// one config named — so the dashboard shows the live engine per agent.
    #[serde(default)]
    pub engine_by_role: std::collections::BTreeMap<String, String>,
    /// The operator (`account@host`) whose runner last ran each role, last-wins —
    /// so an idle agent card can still name which user it belongs to.
    #[serde(default)]
    pub operator_by_role: std::collections::BTreeMap<String, String>,
    /// Characters of prompt SENT per role (system + task), summed — the
    /// measurement that makes prompt trimming data-driven instead of guesswork.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub prompt_chars_by_role: std::collections::BTreeMap<String, u64>,
    /// Runs whose file writes were actually confined (Seatbelt/bwrap).
    #[serde(default)]
    pub confined_runs: u64,
    /// Runs where `workflow.sandbox` was on but confinement was unavailable on
    /// this host, so the run executed unconfined.
    #[serde(default)]
    pub unconfined_requested_runs: u64,
    /// Runs the confinement mechanism refused to apply (macOS Seatbelt's
    /// `sandbox_apply()` denial, COX-B016): the agent never started, so these
    /// are neither confined nor unconfined runs — they are an OS fault.
    #[serde(default)]
    pub sandbox_denied_runs: u64,
    /// Human-readable status of the most recent run's confinement (e.g.
    /// `"confined via bwrap"`, `"unavailable: bwrap not found on PATH"`),
    /// surfaced on the dashboard.
    #[serde(default)]
    pub last_sandbox_status: String,
}

impl Spend {
    /// Average observed cost of one engine run for `role_key` (e.g.
    /// `dev_feature`), or `None` before any metered run of that role.
    #[must_use]
    pub fn avg_role_cost(&self, role_key: &str) -> Option<f64> {
        let runs = *self.runs_by_role.get(role_key)?;
        if runs == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some(
            self.metered_cost_by_role
                .get(role_key)
                .copied()
                .unwrap_or(0.0)
                / runs as f64,
        )
    }
}

/// One operator's slice of the spend, for per-user token accounting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OperatorSpend {
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub runs: u64,
}

/// One cycle's deterministic scorecard — computed from the report and the spend
/// delta at the cycle boundary, zero tokens spent. What "was this cycle worth
/// its cost?" looks like as data instead of a feeling.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CycleScore {
    pub cycle: u64,
    pub at: String,
    /// Engine runs this cycle, and how many produced a recorded outcome
    /// (ticket moved / design saved / bug filed / doc written). The gap is
    /// churn — the 4,439-idle-DOCS-runs class of waste.
    pub runs: u64,
    pub useful: u64,
    /// USD metered this cycle.
    pub cost_usd: f64,
    /// Tickets shipped this cycle (feature done + bug fixed).
    pub shipped: u64,
    /// Engine incidents open at the end of the cycle.
    pub incidents: u64,
    /// Cycle errors reported (excluding informational pauses).
    pub errors: u64,
    /// A–D verdict, precomputed so every consumer grades identically.
    pub grade: String,
    /// Wall-clock seconds spent per phase (role label → secs) this cycle —
    /// where the minutes went, so cadence tuning has data instead of feeling.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub phase_secs: std::collections::BTreeMap<String, u64>,
    /// USD metered per role this cycle — where the money went.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub phase_cost: std::collections::BTreeMap<String, f64>,
    /// Human gate decisions recorded during this cycle, by ticket class
    /// (CXA-F230) — the agent-side scorecard's mirror of the operator's own
    /// attention. serde-defaulted so old scorecards load clean.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub attention_by_area: std::collections::BTreeMap<String, u64>,
    /// Same delta by intervention kind (`ready_approve`, `verify_pass`, …).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub attention_by_kind: std::collections::BTreeMap<String, u64>,
}

impl CycleScore {
    /// Deterministic grade: shipped work is an A; useful-majority activity a B;
    /// idle-but-clean a C; churn or incidents a D.
    #[must_use]
    pub fn grade_of(shipped: u64, runs: u64, useful: u64, incidents: u64, errors: u64) -> String {
        if incidents > 0 || (runs >= 4 && useful == 0) {
            "D".to_owned()
        } else if shipped > 0 {
            "A".to_owned()
        } else if runs > 0 && useful.saturating_mul(2) >= runs && errors == 0 {
            "B".to_owned()
        } else {
            "C".to_owned()
        }
    }
}

/// A sprint (scrum mode): a fixed window of cycles with a goal and a committed
/// set of tickets. Kanban mode leaves this `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sprint {
    pub number: u32,
    pub goal: String,
    pub started_cycle: u64,
    pub length_cycles: u64,
    pub committed: Vec<TicketId>,
    /// When this sprint opened (RFC3339). Rollover requires BOTH the cycle
    /// window and a minimum wall-clock age: cycles shrank from ~30 min to ~90 s
    /// as the loop got faster, and a cycle-only window burned through 500
    /// seven-minute "sprints" in two days — ceremony noise with no meaning.
    #[serde(default)]
    pub started_at: String,
    /// Bug-burn floor mirrored from `WorkflowConfig` when this sprint opened
    /// (CXA-F028). Selection reads it from HERE so the DEV scope stays a pure
    /// function of persisted state — config never leaks into selection call
    /// sites. `None` (old snapshots) = burn every open bug, exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bug_burn_floor: Option<coxagent_domain::Priority>,
}

/// A sprint prepared ahead of time (by the PO or a person) and queued to run
/// after the current one. Rollover consumes the queue front-first: its goal
/// and ticket set become the next sprint's, so planning can run several
/// sprints ahead of execution. An empty queue leaves rollover exactly as it
/// always was (goal chip, then capacity-based auto-commit).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedSprint {
    /// Stable id unique within the queue (monotonic; survives reorders).
    pub id: u64,
    pub goal: String,
    /// Tickets picked for this sprint. Validated again at rollover — shipped
    /// or deleted tickets are silently skipped.
    #[serde(default)]
    pub tickets: Vec<TicketId>,
    #[serde(default)]
    pub created_at: String,
    /// Who queued it ("po" for the agent, else a username).
    #[serde(default)]
    pub by: String,
}

/// Cumulative engine-health counters for one agent role — errors and
/// timeout-class failures with the most recent message, so the Agents view
/// can say "BA is failing 40% of runs on this model" instead of a person
/// grepping hub.log for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleHealth {
    pub errors: u64,
    pub timeouts: u64,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub last_error_at: String,
}

/// A closed sprint's outcome — the velocity history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SprintRecord {
    pub number: u32,
    pub goal: String,
    pub committed: usize,
    pub done: usize,
    pub at: String,
}

/// The SA agent's latest review verdict on an open pull request — surfaced in
/// the Review tab so the user sees the assessment before merging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReview {
    pub number: u64,
    /// `"approve"` or `"request_changes"`.
    pub decision: String,
    pub summary: String,
    pub at: String,
    /// Head commit the verdict was rendered against. A PR whose head has not
    /// moved since a request-changes needs no re-review — re-judging the same
    /// commits burns an engine call to repeat the same comment.
    #[serde(default)]
    pub head_sha: String,
    /// Seconds from the PR's creation to this verdict — THE dispatch-model
    /// health metric (spec trigger: median > 10 min for a week). Computed on
    /// the hub from the mirrored open-PR record; `None` when unknowable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_secs: Option<u64>,
}

/// One ticket attachment (a PD design image, a screenshot): the record the UI
/// lists. The bytes live in blob storage (`StoragePort`) under `key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketAttachment {
    /// Human file name ("login-mockup.svg").
    pub name: String,
    /// Opaque storage key; echoed back to the attachment fetch endpoint.
    pub key: String,
    /// MIME type ("image/svg+xml").
    pub content_type: String,
    /// Who attached it ("PD", or a username).
    pub by: String,
    /// RFC3339 timestamp.
    pub at: String,
}

/// A product milestone — a named delivery target that one or more sprints work
/// toward. `target_version` is the release that marks it reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Milestone {
    pub name: String,
    pub goal: String,
    /// Release version that completes this milestone, e.g. "0.5.0".
    pub target_version: String,
    /// Whether the milestone's scope is done and the release can proceed.
    /// Set by the PO or a human — not derived, because the code version alone
    /// does not mean the work is shippable.
    #[serde(default)]
    pub goal_complete: bool,
    /// True after the release pipeline has tagged and documented the milestone.
    /// Prevents re-releasing the same milestone when the runner restarts or
    /// retries.
    #[serde(default)]
    pub fulfilled: bool,
}

/// Why one attempt at a ticket failed, in a form later agents can reason over
/// instead of pattern-matching prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptFailure {
    /// 1-based attempt number.
    pub attempt: u32,
    /// Which layer the work died at.
    pub layer: FailureLayer,
    /// The gate or step that rejected it (`clippy`, `regression-test`,
    /// `tests`, `engine`), for routing and for the human digest.
    pub gate: String,
    /// The decisive detail, already trimmed (a lint line, an assertion).
    pub detail: String,
    /// Repo-relative files implicated, when the gate knows them.
    #[serde(default)]
    pub files: Vec<String>,
}

/// The layer an attempt died at — the thing that decides WHO can unstick it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureLayer {
    /// The requirement could not be built against (BA's problem).
    Spec,
    /// A mechanical quality gate rejected otherwise-sound work.
    Gate,
    /// The approach itself does not work (SA's problem).
    Design,
    /// Auth, network, capacity — nobody's fault, never counted.
    Infra,
}

/// One piece of Definition-of-Done evidence attached to a ticket: proof the
/// change actually works in its own context (UI → a real screenshot; API → a
/// real request/response; or an explicit waiver when the host can't collect).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// `screenshot` | `api` | `test` | `waived`
    pub kind: String,
    pub label: String,
    /// Screenshot: repo-relative path. API: capped request/response text.
    pub detail: String,
    pub at: String,
}

/// Orchestrator self-tuning state, derived from the evals each day.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tuning {
    /// Quality brake: retry churn per shipped ticket ran hot, so DEV-FEATURE
    /// pauses and the team burns down bugs until churn recovers.
    #[serde(default)]
    pub bugs_first: bool,
    /// Intake brake: the backlog outgrew throughput, so BA proposals pause
    /// until the queue drains.
    #[serde(default)]
    pub skip_ba: bool,
    /// Human burn mode (CXA-F030): a person pauses feature work and the team
    /// burns down open bugs until the exit gate releases it. Unlike
    /// `bugs_first` — recomputed from the evals each day — this is a human
    /// decision the loop must honour and never overwrite.
    #[serde(default)]
    pub burn_mode: bool,
    /// The burn mode's explicit exit gate: once the open-bug count is at or
    /// below this, the mode clears itself and features resume. Absent means
    /// no numeric gate — the mode then holds until switched off by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burn_until_bugs_le: Option<u32>,
    /// The day (`YYYY-MM-DD`) tuning was last evaluated.
    #[serde(default)]
    pub last_eval_day: String,
}

impl Tuning {
    #[must_use]
    pub fn is_default(&self) -> bool {
        !self.bugs_first
            && !self.skip_ba
            && !self.burn_mode
            && self.burn_until_bugs_le.is_none()
            && self.last_eval_day.is_empty()
    }
}

/// A bounded operator freeze/override riding on one self-tuning brake
/// (CXA-F238). Keyed by brake field name (`bugs_first` / `skip_ba`) in
/// [`ProjectState::tuning_overrides`]; one hold per brake — setting a hold
/// replaces the previous one.
///
/// Unlike `Tuning::burn_mode` (a global, unbounded human decision), a hold is
/// per-brake, always carries an expiry bound, and never touches
/// `decide_tuning`'s hysteresis math: it composes AFTER the autonomous
/// decision each pass, so expiry hands control straight back to the loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrakeHold {
    /// `Some(v)` pins the brake to `v` (override); `None` freezes it at the
    /// value it had when the hold was set (hold) — autonomous recomputation
    /// suspended, not reversed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_value: Option<bool>,
    /// Why the operator intervened — the human-readable half of the audit.
    #[serde(default)]
    pub reason: String,
    /// Who set it (username, or `operator` in open mode).
    #[serde(default)]
    pub actor: String,
    /// RFC3339 moment the hold was set.
    #[serde(default)]
    pub at: String,
    /// RFC3339 bound past which the hold no longer applies. Always present:
    /// an unparseable/absent bound is treated as expired (fail closed toward
    /// autonomy), so governance can never be stranded by a corrupt field.
    #[serde(default)]
    pub expires_at: String,
}

/// One append-only brake-cockpit audit entry (CXA-F238): a single brake
/// field's value change with who caused it and why. `from`/`to` are the
/// field-wise values, so the trail reads as a direction (`false→true`) rather
/// than prose. Bounded — see [`ProjectState::record_tuning_change`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningAuditEntry {
    /// RFC3339 moment the change landed.
    #[serde(default)]
    pub at: String,
    /// Who caused it: `"SM"` for the autonomous pass, the username for an
    /// operator action.
    #[serde(default)]
    pub actor: String,
    /// Which pass wrote it: `self_tune` (daily autonomous), `hold` (operator
    /// set a hold), `clear` (operator released one), `expiry` (bound elapsed).
    #[serde(default)]
    pub source: String,
    /// Brake field name (`bugs_first` / `skip_ba`).
    #[serde(default)]
    pub brake: String,
    /// Value before the change.
    #[serde(default)]
    pub from: bool,
    /// Value after the change.
    #[serde(default)]
    pub to: bool,
    /// Why — the same wording the SM announcement uses for autonomous flips,
    /// the operator's own reason for holds.
    #[serde(default)]
    pub reason: String,
    /// The hold's expiry window, when this entry concerns a hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

#[cfg(test)]
mod attempt_failure_tests {
    use super::{AttemptFailure, FailureLayer, ProjectState};

    fn failure(attempt: u32, gate: &str) -> AttemptFailure {
        AttemptFailure {
            attempt,
            layer: FailureLayer::Gate,
            gate: gate.to_owned(),
            detail: "d".to_owned(),
            files: vec!["crates/app/src/lib.rs".to_owned()],
        }
    }

    #[test]
    fn failures_are_kept_per_ticket_and_bounded() {
        let mut s = ProjectState::default();
        for n in 1..=9 {
            s.record_attempt_failure("COX-B006", failure(n, "clippy"));
        }
        s.record_attempt_failure("COX-B007", failure(1, "tests"));
        let log = s.attempt_failures("COX-B006");
        assert_eq!(log.len(), 6, "old attempts age out");
        assert_eq!(log[0].attempt, 4, "the oldest kept is the 4th");
        assert_eq!(s.attempt_failures("COX-B007").len(), 1);
        assert!(s.attempt_failures("COX-NONE").is_empty());
    }

    #[test]
    fn state_written_before_this_field_existed_still_loads() {
        // Projects on disk predate the structured log; a missing key must not
        // fail the load and strand a whole project.
        let mut doc = serde_json::to_value(ProjectState::default()).expect("serialize");
        doc.as_object_mut()
            .expect("object")
            .remove("ticket_failures");
        let back: ProjectState = serde_json::from_value(doc).expect("load legacy state");
        assert!(back.ticket_failures.is_empty());
    }
}

#[cfg(test)]
mod sprint_floor_tests {
    use super::{ProjectState, Sprint};
    use coxagent_domain::Priority;

    fn sprint_with_floor(floor: Option<Priority>) -> Sprint {
        Sprint {
            number: 1,
            goal: "burn".to_owned(),
            started_cycle: 1,
            length_cycles: 10,
            committed: Vec::new(),
            started_at: String::new(),
            bug_burn_floor: floor,
        }
    }

    #[test]
    fn the_floor_round_trips_through_the_sprint_record() {
        for floor in [None, Some(Priority::Low), Some(Priority::High)] {
            let s = ProjectState {
                sprint: Some(sprint_with_floor(floor)),
                ..ProjectState::default()
            };
            let doc = serde_json::to_value(&s).expect("serialize");
            let back: ProjectState = serde_json::from_value(doc).expect("deserialize");
            assert_eq!(back.sprint.expect("sprint").bug_burn_floor, floor);
        }
    }

    #[test]
    fn a_sprint_snapshot_from_before_the_floor_still_loads() {
        // Projects persisted before CXA-F028 have no `bug_burn_floor` key on
        // their sprint; the load must give None (burn everything), not fail.
        let mut doc = serde_json::to_value(ProjectState {
            sprint: Some(sprint_with_floor(Some(Priority::High))),
            ..ProjectState::default()
        })
        .expect("serialize");
        doc.as_object_mut()
            .expect("object")
            .get_mut("sprint")
            .expect("sprint key")
            .as_object_mut()
            .expect("sprint object")
            .remove("bug_burn_floor");
        let back: ProjectState = serde_json::from_value(doc).expect("legacy sprint loads");
        assert_eq!(back.sprint.expect("sprint").bug_burn_floor, None);
    }

    #[test]
    fn an_absent_floor_is_not_serialized_into_new_snapshots() {
        // Old stores stay byte-shaped like before when the floor is unused.
        let doc = serde_json::to_value(ProjectState {
            sprint: Some(sprint_with_floor(None)),
            ..ProjectState::default()
        })
        .expect("serialize");
        assert!(
            doc.pointer("/sprint/bug_burn_floor").is_none(),
            "None must not add a key old readers never saw"
        );
    }
}

#[cfg(test)]
mod daily_job_tests {
    use super::ProjectState;

    #[test]
    fn a_daily_job_is_remembered_per_day_and_per_job() {
        let mut s = ProjectState::default();
        s.daily_jobs
            .insert("standup".to_owned(), "2026-07-29".to_owned());
        // Same job, same day: already done. Same day, other job: not.
        assert_eq!(
            s.daily_jobs.get("standup").map(String::as_str),
            Some("2026-07-29")
        );
        assert!(!s.daily_jobs.contains_key("po-milestones"));
        // A new day replaces the stamp rather than accumulating entries.
        s.daily_jobs
            .insert("standup".to_owned(), "2026-07-30".to_owned());
        assert_eq!(s.daily_jobs.len(), 1);
        assert_eq!(
            s.daily_jobs.get("standup").map(String::as_str),
            Some("2026-07-30")
        );
    }

    #[test]
    fn state_without_the_field_still_loads() {
        let mut doc = serde_json::to_value(ProjectState::default()).expect("serialize");
        doc.as_object_mut().expect("object").remove("daily_jobs");
        doc.as_object_mut()
            .expect("object")
            .remove("engine_incidents");
        let back: ProjectState = serde_json::from_value(doc).expect("legacy state loads");
        assert!(back.daily_jobs.is_empty() && back.engine_incidents.is_empty());
    }
}

#[cfg(test)]
mod burn_mode_tests {
    use super::{ProjectState, Tuning};
    use crate::ports::outbound::{mutate_state, StateStorePort};
    use crate::PortError;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// In-memory store — the round-trip needs no filesystem.
    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }

    #[async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().map_err(poison)?.clone())
        }
        async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
            state.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().map_err(poison)? = state.clone();
            Ok(())
        }
    }

    fn poison<T>(_: std::sync::PoisonError<T>) -> PortError {
        PortError::Backend("lock poisoned".to_owned())
    }

    #[test]
    fn is_default_covers_the_burn_mode_fields() {
        assert!(Tuning::default().is_default());
        let engaged = Tuning {
            burn_mode: true,
            ..Tuning::default()
        };
        assert!(!engaged.is_default());
        let gated = Tuning {
            burn_until_bugs_le: Some(0),
            ..Tuning::default()
        };
        assert!(
            !gated.is_default(),
            "a stored exit gate must survive: `tuning` is skipped from the persisted \
             document only while is_default() holds"
        );
    }

    #[test]
    fn state_written_before_burn_mode_existed_still_loads() {
        // Projects on disk predate the burn-mode fields; a missing key must
        // not fail the load and strand a whole project.
        let mut doc = serde_json::to_value(ProjectState::default()).expect("serialize");
        doc.as_object_mut().expect("object").remove("tuning");
        let back: ProjectState = serde_json::from_value(doc).expect("load legacy state");
        assert!(!back.tuning.burn_mode);
        assert!(back.tuning.burn_until_bugs_le.is_none());
    }

    #[tokio::test]
    async fn burn_mode_set_through_the_store_round_trips() {
        let store = Arc::new(MemStore::default());
        mutate_state(store.as_ref(), |s| {
            s.tuning.burn_mode = true;
            s.tuning.burn_until_bugs_le = Some(2);
            Ok(())
        })
        .await
        .expect("set through the store");
        let s = store.load().await.expect("load");
        assert!(s.tuning.burn_mode);
        assert_eq!(s.tuning.burn_until_bugs_le, Some(2));
        // And it must be PERSISTED, not skipped as default tuning.
        let doc = serde_json::to_value(&s).expect("serialize");
        assert_eq!(doc["tuning"]["burn_mode"], serde_json::json!(true));
        assert_eq!(doc["tuning"]["burn_until_bugs_le"], serde_json::json!(2));
    }
}
