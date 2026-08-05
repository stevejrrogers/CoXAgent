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
    /// Runs whose file writes were actually confined (Seatbelt/bwrap).
    #[serde(default)]
    pub confined_runs: u64,
    /// Runs where `workflow.sandbox` was on but confinement was unavailable on
    /// this host, so the run executed unconfined.
    #[serde(default)]
    pub unconfined_requested_runs: u64,
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

/// A sprint (scrum mode): a fixed window of cycles with a goal and a committed
/// set of tickets. Kanban mode leaves this `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sprint {
    pub number: u32,
    pub goal: String,
    pub started_cycle: u64,
    pub length_cycles: u64,
    pub committed: Vec<TicketId>,
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
    /// The day (`YYYY-MM-DD`) tuning was last evaluated.
    #[serde(default)]
    pub last_eval_day: String,
}

impl Tuning {
    #[must_use]
    pub fn is_default(&self) -> bool {
        !self.bugs_first && !self.skip_ba && self.last_eval_day.is_empty()
    }
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
