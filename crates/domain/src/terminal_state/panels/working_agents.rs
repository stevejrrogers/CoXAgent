//! `#ov-working` — the busy-agent chips' terminal-state value types
//! (CXA-B253, part 2/3; panel 3 of the CXA-B228 enumeration
//! [`OverviewPanelId::OvWorking`]).
//!
//! Evidence: `renderOvWorking` paints the chips
//! (`crates/presentation/src/web/js/core.js:1021–1039`), merging `/workers`
//! rows (filtering `leader|worker|idle` roles) with the `/agent-liveness`
//! liveness records. Types only — no wiring (that is part 3).
//!
//! Core-state projection: chips showing is `Succeeded`; a zero-chip moment
//! means NO agent is busy — a real, healthy state, so it maps to `Skipped`
//! with [`FailureReason::NoData`] and the domain's default fill hint.
//! `Cancelled` is unrepresentable (the panel never self-cancels), so the
//! reverse conversion refuses it.

use crate::DomainError;
use crate::terminal_state::{FailureReason, FillHint, TerminalState};

/// One busy-agent chip as the panel renders it (`core.js:1032–1039`):
/// role, ticket alias, and the agent identity — with who/when attribution,
/// because a chip that can't say who is working or since when is a bare
/// label, not a terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BusyAgent {
    /// The agent's role as the chip renders it (`ARCHITECT`, `DEV-BUG`… —
    /// the raw role with `_` → `-` applied).
    pub role: String,
    /// The ticket alias the agent is working (`CXA-B253`, empty = no claim).
    pub ticket: String,
    /// The worker/agent identity (`w-1`, `arch-1`…).
    pub who: String,
    /// When the chip's data was read (RFC-3339 string; part 3 owns
    /// timestamps).
    pub since: String,
}

impl BusyAgent {
    /// Validate and construct one chip — role and `who` must be non-blank.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `role` or `who` is blank
    /// (`ticket` may be empty: an idle-but-visible agent has no claim).
    pub fn new(
        role: impl Into<String>,
        ticket: impl Into<String>,
        who: impl Into<String>,
        since: impl Into<String>,
    ) -> Result<Self, crate::DomainError> {
        Ok(Self {
            role: non_blank(role, "busy_agent_role")?,
            ticket: ticket.into(),
            who: non_blank(who, "busy_agent_who")?,
            since: non_blank(since, "busy_agent_since")?,
        })
    }
}

/// The busy-agent chips' terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorkingAgentsTerminalState {
    /// ≥1 busy agent, each chip with its attribution.
    Succeeded {
        /// When the chip set was fetched (RFC-3339 string).
        fetched_at: String,
        /// Who produced the set (`GET /api/workers`).
        source: String,
        /// The chips, in strip order. Never empty here — that state is
        /// [`WorkingAgentsTerminalState::Skipped`].
        agents: Vec<BusyAgent>,
    },
    /// No busy agent right now — the healthy, nothing-running state.
    Skipped {
        /// Why the strip is empty (always [`FailureReason::NoData`]).
        reason: FailureReason,
        /// The mandatory "what fills this panel" hint.
        hint: FillHint,    },
    /// The chip set could not be read.
    Failed {
        /// Non-blank human cause (core invariant).
        cause: String,
        /// Whether re-polling `/workers` may help.
        retryable: bool,
    },
}

impl WorkingAgentsTerminalState {
    /// Terminal success: a non-empty chip set.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `source` is blank or the
    /// agent list is empty (that is the `Skipped` state, not a success).
    pub fn succeeded(
        fetched_at: String,
        source: impl Into<String>,
        agents: Vec<BusyAgent>,
    ) -> Result<Self, crate::DomainError> {
        if agents.is_empty() {
            return Err(crate::DomainError::Empty {
                field: "working_agents_agents",
            });
        }
        Ok(Self::Succeeded {
            fetched_at,
            source: non_blank(source, "working_agents_source")?,
            agents,
        })
    }

    /// Terminal emptiness: nothing running, with attribution and the
    /// default hint.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `source` is blank.
    #[must_use]
    pub fn skipped() -> Self {
        Self::Skipped {
            reason: FailureReason::NoData,
            hint: FillHint::for_reason(FailureReason::NoData),
        }
    }


    /// Terminal failure.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `cause` is blank.
    pub fn failed(
        cause: impl Into<String>,
        retryable: bool,
    ) -> Result<Self, crate::DomainError> {
        Ok(Self::Failed {
            cause: non_blank(cause, "working_agents_cause")?,
            retryable,
        })
    }
}

impl From<&WorkingAgentsTerminalState> for TerminalState {
    fn from(panel: &WorkingAgentsTerminalState) -> Self {
        match panel {
            WorkingAgentsTerminalState::Succeeded {
                fetched_at,
                source,
                ..
            } => Self::Succeeded {
                fetched_at: fetched_at.clone(),
                source: source.clone(),
            },
            WorkingAgentsTerminalState::Skipped { reason, hint } => Self::Skipped {
                reason: *reason,
                hint: hint.clone(),
            },
            WorkingAgentsTerminalState::Failed {
                cause,
                retryable,
            } => Self::Failed {
                cause: cause.clone(),
                retryable: *retryable,
            },
        }
    }
}

impl From<WorkingAgentsTerminalState> for TerminalState {
    fn from(panel: WorkingAgentsTerminalState) -> Self {
        Self::from(&panel)
    }
}

impl TryFrom<&TerminalState> for WorkingAgentsTerminalState {
    type Error = crate::DomainError;

    fn try_from(core: &TerminalState) -> Result<Self, Self::Error> {
        match core {
            TerminalState::Skipped { reason, hint } if *reason == FailureReason::NoData => Ok(Self::Skipped { reason: *reason, hint: hint.clone() }),
            TerminalState::Failed { cause, retryable } => Ok(Self::Failed { cause: cause.clone(), retryable: *retryable }),
            // Succeeded core cannot rebuild the payload (documented
            // projection: Skipped/Failed/Cancelled reconstruct at their zero
            // point; TimedOut and non-NoData Skipped are unrepresentable) —
            // the caller uses the panel's own constructors instead.
            _ => Err(crate::DomainError::Empty {
                field: "terminal_state_not_representable",
            }),
        }
    }
}

/// Refuse blank payloads so an unattributable state is unrepresentable — the
/// [`crate::terminal_state`] module-wide invariant.
fn non_blank(raw: impl Into<String>, field: &'static str) -> Result<String, DomainError> {
    let raw = raw.into();
    if raw.trim().is_empty() {
        return Err(DomainError::Empty { field });
    }
    Ok(raw)
}

impl super::PanelTerminalState for WorkingAgentsTerminalState {
    fn panel_id(&self) -> crate::overview_panel::OverviewPanelId {
        crate::overview_panel::OverviewPanelId::OvWorking
    }

    fn terminal_state(&self) -> TerminalState {
        TerminalState::from(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(n: u32) -> BusyAgent {
        BusyAgent::new(
            "DEV-FEATURE",
            format!("CXA-B{n}"),
            "w-1",
            "2026-02-24T10:00:00Z",
        )
        .expect("role/who are non-blank")
    }

    #[test]
    fn an_empty_agent_list_is_not_a_success() {
        assert_eq!(
            WorkingAgentsTerminalState::succeeded("2026-02-24T10:00:00Z".to_owned(), "/workers", vec![]),
            Err(crate::DomainError::Empty {
                field: "working_agents_agents"
            })
        );
    }

    #[test]
    fn a_blank_role_is_refused() {
        assert!(BusyAgent::new("  ", "CXA-B253", "w-1", "2026-02-24T10:00:00Z").is_err());
    }

    #[test]
    fn skipped_is_no_data_with_the_default_hint() {
        let state =
            WorkingAgentsTerminalState::skipped();
        match &state {
            WorkingAgentsTerminalState::Skipped { reason, hint, .. } => {
                assert_eq!(*reason, FailureReason::NoData);
                assert_eq!(hint.as_str(), FillHint::for_reason(FailureReason::NoData).as_str());
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn panel_to_core_round_trips() {
        let panel = WorkingAgentsTerminalState::succeeded(
            "2026-02-24T10:00:00Z".to_owned(),
            "/workers",
            vec![agent(253)],
        )
        .expect("agents are non-empty");
        let core = TerminalState::from(&panel);
        assert!(matches!(core, TerminalState::Succeeded { .. }));
        let back = WorkingAgentsTerminalState::try_from(&core);
        assert!(back.is_err(), "Succeeded core cannot rebuild the payload — documented projection");
    }

    #[test]
    fn skipped_round_trips() {
        let panel =
            WorkingAgentsTerminalState::skipped();
        let core = TerminalState::from(panel.clone());
        assert_eq!(WorkingAgentsTerminalState::try_from(&core), Ok(panel));
    }

    #[test]
    fn a_cancelled_core_state_cannot_reconstruct_busy_agents() {
        assert!(WorkingAgentsTerminalState::try_from(&TerminalState::Cancelled).is_err());
    }
}
