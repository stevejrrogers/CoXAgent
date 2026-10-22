//! Terminal-state rendering for the cycle/deploy flow (CXA-B260,
//! subtask 3/3 of the CXA-B173a/B196 lineage).
//!
//! Pure presentation: every function here only FORMATS what the application
//! layer already returned ([`DeployStatus`] on [`ProjectState::deploy`]) —
//! no IO, no decisions, no process spawns. A deploy attempt that ended has a
//! label; a FAILED attempt ALWAYS carries its WHY (the recorded summary — a
//! silent bare terminal state is not acceptable); a flow that has not ended
//! yet gets an explicit empty-state hint instead of a blank line.

use coxagent_application::state::DeployStatus;

/// Label for the not-yet-terminal case: the flow can end without a terminal
/// state simply because nothing ran — say so instead of printing nothing.
pub const LABEL_NOT_STARTED: &str = "not started";
/// Empty-state hint shown alongside [`LABEL_NOT_STARTED`].
pub const HINT_NOT_STARTED: &str =
    "no deploy run yet — one fires automatically after the cycle's TEST gate passes";

/// Terminal label for a successful deploy attempt.
pub const LABEL_SUCCEEDED: &str = "succeeded";
/// Terminal label for a failed deploy attempt (failure class — always
/// rendered together with its reason, see [`deploy_terminal_lines`]).
pub const LABEL_FAILED: &str = "FAILED";

/// Placeholder used when a FAILED attempt somehow recorded no reason, so the
/// WHY slot is never silently empty.
const NO_REASON_RECORDED: &str = "(no reason recorded)";

/// Classify the deploy flow's terminal state from the use-case-owned snapshot.
///
/// Exhaustive ON PURPOSE: presentation must name every outcome, so a new
/// field on [`DeployStatus`] forces this match to be revisited rather than
/// silently unrendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployTerminal<'a> {
    /// `state.deploy` absent — nothing has run yet.
    NotStarted,
    /// The attempt reached its terminal state successfully.
    Succeeded(&'a DeployStatus),
    /// The attempt failed; `reason` is WHY, and is never dropped on the floor.
    Failed { attempt: &'a DeployStatus, reason: &'a str },
}

impl<'a> DeployTerminal<'a> {
    /// Classification of the last deploy attempt (pure, over real state).
    #[must_use]
    pub fn classify(deploy: Option<&'a DeployStatus>) -> Self {
        match deploy {
            None => Self::NotStarted,
            Some(attempt) if attempt.ok => Self::Succeeded(attempt),
            // A FAILED attempt always keeps its reason visible: blank summaries
            // degrade to an explicit placeholder rather than an empty WHY.
            Some(attempt) => {
                let reason = if attempt.summary.trim().is_empty() {
                    NO_REASON_RECORDED
                } else {
                    attempt.summary.as_str()
                };
                Self::Failed { attempt, reason }
            }
        }
    }

    /// The user-facing label for this terminal state.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotStarted => LABEL_NOT_STARTED,
            Self::Succeeded(_) => LABEL_SUCCEEDED,
            Self::Failed { .. } => LABEL_FAILED,
        }
    }
}

/// Render the deploy-flow terminal state as presentation lines for the CLI /
/// API text report (`render_report`).
///
/// Shape (one line per fact, indented under the `deploy` key):
///
/// ```text
/// deploy         : succeeded
///   when         : 2026-09-15T01:07:28Z
///   commit       : ed8fca62
///   summary      : released 0.4.1
/// ```
///
/// Failure adds the mandatory WHY line; the empty state replaces the facts
/// with the not-started hint.
#[must_use]
pub fn deploy_terminal_lines(deploy: Option<&DeployStatus>) -> Vec<String> {
    match DeployTerminal::classify(deploy) {
        DeployTerminal::NotStarted => {
            vec![format!("deploy         : {LABEL_NOT_STARTED}\n                 ({HINT_NOT_STARTED})")]
        }
        DeployTerminal::Succeeded(attempt) => {
            let mut lines = vec![
                format!("deploy         : {LABEL_SUCCEEDED}"),
                format!("  when         : {}", attempt.at),
            ];
            if let Some(sha) = &attempt.commit_sha {
                lines.push(format!("  commit       : {sha}"));
            }
            if !attempt.summary.trim().is_empty() {
                lines.push(format!("  summary      : {}", attempt.summary));
            }
            push_health_line(&mut lines, attempt);
            lines
        }
        DeployTerminal::Failed { attempt, reason } => {
            let mut lines = vec![
                format!("deploy         : {LABEL_FAILED}"),
                format!("  why          : {reason}"),
            ];
            if let Some(sha) = &attempt.commit_sha {
                lines.push(format!("  commit       : {sha}"));
            }
            push_health_line(&mut lines, attempt);
            lines
        }
    }
}

/// Health-probe outcome rendered as attribution WHEN IT EXISTS (COX-F005);
/// suppressed otherwise — absence means the probe never ran, which is stated
/// nowhere rather than invented.
fn push_health_line(lines: &mut Vec<String>, attempt: &DeployStatus) {
    if let Some(hc) = &attempt.health_check {
        if hc.passed {
            lines.push("  health       : passed".to_string());
        } else {
            // Failure-class detail inside the failure class: still a WHY.
            let status = hc
                .http_status
                .map(|code| format!(" (HTTP {code})"))
                .unwrap_or_default();
            lines.push(format!("  health       : failed{status}"));
        }
    }
}

#[cfg(test)]
mod tests;
