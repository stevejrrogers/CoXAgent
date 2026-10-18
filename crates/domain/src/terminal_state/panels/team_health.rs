//! `#ov-health` — the team-health cards' terminal-state value types
//! (CXA-B253, part 2/3; panel 5 of the CXA-B228 enumeration
//! [`OverviewPanelId::OvHealth`]).
//!
//! Evidence: `renderHealth` paints one card per agent role
//! (`crates/presentation/src/web/js/chat.js:1761+`), reading the role map
//! the state snapshot carries. Types only — no wiring (that is part 3).
//!
//! Core-state projection: cards showing is `Succeeded`; an empty role map is
//! a real, healthy state (no agents have ever run) → `Skipped` with
//! [`FailureReason::NoData`] and the default fill hint; `Failed` is the
//! snapshot read failing.

use crate::terminal_state::{FailureReason, FillHint, TerminalState};
use crate::DomainError;

/// One agent's health card as the panel renders it (`chat.js:1761+`):
/// the role, its lifecycle status, and the counts the card displays — plus
/// who/when attribution, because a red card that can't say why is a bare
/// state, not a terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct AgentHealthCard {
    /// The agent role the card is for (`ARCHITECT`, `DEV-FEATURE`…).
    pub role: String,
    /// The card's status word (`ok`, `stalled`, `error`…).
    pub status: String,
    /// The count lines the card displays (pre-formatted label → value).
    pub counters: Vec<(String, String)>,
    /// Who/what set this status (`gate`, `watchdog`, `idle-timeout`…).
    pub attributed_to: String,
    /// When the status was set (RFC-3339 string; part 3 owns timestamps).
    pub at: String,
}

impl AgentHealthCard {
    /// Validate and construct one card — role, status and attribution must
    /// be non-blank.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when any of those fields is
    /// blank.
    pub fn new(
        role: impl Into<String>,
        status: impl Into<String>,
        counters: Vec<(String, String)>,
        attributed_to: impl Into<String>,
        at: impl Into<String>,
    ) -> Result<Self, crate::DomainError> {
        Ok(Self {
            role: non_blank(role, "agent_health_role")?,
            status: non_blank(status, "agent_health_status")?,
            counters,
            attributed_to: non_blank(attributed_to, "agent_health_attributed_to")?,
            at: non_blank(at, "agent_health_at")?,
        })
    }
}

/// The team-health cards' terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TeamHealthTerminalState {
    /// ≥1 agent card, each with its attribution.
    Succeeded {
        /// When the cards were computed (RFC-3339 string).
        fetched_at: String,
        /// Who produced them (the state snapshot's role map).
        source: String,
        /// The cards, in the snapshot's role order. Never empty here — that
        /// state is [`TeamHealthTerminalState::Skipped`].
        cards: Vec<AgentHealthCard>,
    },
    /// No agent has ever run — a real, healthy state.
    Skipped {
        /// Why the strip is empty (always [`FailureReason::NoData`]).
        reason: FailureReason,
        /// The mandatory "what fills this panel" hint (the core's own —
        /// panels compose the core, they do not re-invent attribution).
        hint: FillHint,
    },
    /// The role map read failed.
    Failed {
        /// Non-blank human cause (core invariant).
        cause: String,
        /// Whether re-polling the snapshot may help.
        retryable: bool,
    },
}

impl TeamHealthTerminalState {
    /// Terminal success: a non-empty card set.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `source` is blank or the
    /// card list is empty (that is the `Skipped` state, not a success).
    pub fn succeeded(
        fetched_at: String,
        source: impl Into<String>,
        cards: Vec<AgentHealthCard>,
    ) -> Result<Self, crate::DomainError> {
        if cards.is_empty() {
            return Err(crate::DomainError::Empty {
                field: "team_health_cards",
            });
        }
        Ok(Self::Succeeded {
            fetched_at,
            source: non_blank(source, "team_health_source")?,
            cards,
        })
    }

    /// Terminal emptiness: no agent yet, with attribution and the default
    /// hint.
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
    pub fn failed(cause: impl Into<String>, retryable: bool) -> Result<Self, crate::DomainError> {
        Ok(Self::Failed {
            cause: non_blank(cause, "team_health_cause")?,
            retryable,
        })
    }
}

impl From<&TeamHealthTerminalState> for TerminalState {
    fn from(panel: &TeamHealthTerminalState) -> Self {
        match panel {
            TeamHealthTerminalState::Succeeded {
                fetched_at, source, ..
            } => Self::Succeeded {
                fetched_at: fetched_at.clone(),
                source: source.clone(),
            },
            TeamHealthTerminalState::Skipped { reason, hint } => Self::Skipped {
                reason: *reason,
                hint: hint.clone(),
            },
            TeamHealthTerminalState::Failed { cause, retryable } => Self::Failed {
                cause: cause.clone(),
                retryable: *retryable,
            },
        }
    }
}

impl From<TeamHealthTerminalState> for TerminalState {
    fn from(panel: TeamHealthTerminalState) -> Self {
        Self::from(&panel)
    }
}

impl TryFrom<&TerminalState> for TeamHealthTerminalState {
    type Error = crate::DomainError;

    fn try_from(core: &TerminalState) -> Result<Self, Self::Error> {
        match core {
            TerminalState::Skipped { reason, hint } if *reason == FailureReason::NoData => {
                Ok(Self::Skipped {
                    reason: *reason,
                    hint: hint.clone(),
                })
            }
            TerminalState::Failed { cause, retryable } => Ok(Self::Failed {
                cause: cause.clone(),
                retryable: *retryable,
            }),
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

impl super::PanelTerminalState for TeamHealthTerminalState {
    fn panel_id(&self) -> crate::overview_panel::OverviewPanelId {
        crate::overview_panel::OverviewPanelId::OvHealth
    }

    fn terminal_state(&self) -> TerminalState {
        TerminalState::from(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(n: u32) -> AgentHealthCard {
        AgentHealthCard::new(
            "DEV-FEATURE",
            "ok",
            vec![("claims".to_owned(), format!("{n}"))],
            "gate",
            "2026-02-24T10:00:00Z",
        )
        .expect("role/status/attributed_to are non-blank")
    }

    #[test]
    fn a_blank_status_is_refused() {
        assert!(
            AgentHealthCard::new("DEV-FEATURE", " ", vec![], "gate", "2026-02-24T10:00:00Z")
                .is_err()
        );
    }

    #[test]
    fn an_empty_card_list_is_not_a_success() {
        assert_eq!(
            TeamHealthTerminalState::succeeded(
                "2026-02-24T10:00:00Z".to_owned(),
                "snapshot",
                vec![]
            ),
            Err(crate::DomainError::Empty {
                field: "team_health_cards"
            })
        );
    }

    #[test]
    fn skipped_is_no_data_with_the_default_hint() {
        let state = TeamHealthTerminalState::skipped();
        match &state {
            TeamHealthTerminalState::Skipped { reason, hint, .. } => {
                assert_eq!(*reason, FailureReason::NoData);
                assert_eq!(
                    hint.as_str(),
                    FillHint::for_reason(FailureReason::NoData).as_str()
                );
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn succeeded_to_core_carries_attribution() {
        let panel = TeamHealthTerminalState::succeeded(
            "2026-02-24T10:00:00Z".to_owned(),
            "GET /api/state",
            vec![card(3)],
        )
        .expect("cards are non-empty");
        assert_eq!(
            TerminalState::from(&panel),
            TerminalState::Succeeded {
                fetched_at: "2026-02-24T10:00:00Z".to_owned(),
                source: "GET /api/state".to_owned(),
            }
        );
    }

    #[test]
    fn failed_round_trips() {
        let panel =
            TeamHealthTerminalState::failed("the role map was absent from the snapshot", true)
                .expect("cause is non-blank");
        let core = TerminalState::from(panel.clone());
        assert!(matches!(core, TerminalState::Failed { .. }));
        assert_eq!(TeamHealthTerminalState::try_from(&core), Ok(panel));
    }

    #[test]
    fn a_cancelled_core_state_cannot_reconstruct_team_health() {
        assert!(TeamHealthTerminalState::try_from(&TerminalState::Cancelled).is_err());
    }
}
