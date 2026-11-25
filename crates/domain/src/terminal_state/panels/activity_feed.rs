//! `#ov-activity` — the recent-activity feed's terminal-state value types
//! (CXA-B253, part 2/3; panel 6 of the CXA-B228 enumeration
//! [`OverviewPanelId::OvActivity`]).
//!
//! Evidence: `core.js:814` paints the feed from the snapshot's `activity`
//! array via `actItem` (`core.js:589`), and `core.js:780` skips the feed
//! entirely (with the KPI strip) when no activity exists — a real state the
//! shipped UI already renders. Types only — no wiring (that is part 3).
//!
//! Core-state projection: entries showing is `Succeeded`; an empty feed is
//! `Skipped` with [`FailureReason::NoData`] and the default fill hint;
//! `Failed` is the snapshot read failing.

use crate::terminal_state::{FailureReason, FillHint, TerminalState};
use crate::DomainError;

/// One activity entry as the feed renders it (`core.js:589–596`): who did
/// what, when, and where it landed — attribution IS the payload here, so
/// every field is mandatory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ActivityEntry {
    /// The actor (`ARCHITECT`, `SA`, `human:ana`…).
    pub actor: String,
    /// The event kind (`claimed`, `merged`, `commented`…).
    pub kind: String,
    /// The human one-line summary.
    pub summary: String,
    /// The ticket alias the event is about (`CXA-B253`, optional).
    pub ticket: Option<String>,
    /// When the event happened (RFC-3339 string; part 3 owns timestamps).
    pub at: String,
}

impl ActivityEntry {
    /// Validate and construct one entry — actor, kind, summary and `at`
    /// must be non-blank.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when any of those fields is
    /// blank.
    pub fn new(
        actor: impl Into<String>,
        kind: impl Into<String>,
        summary: impl Into<String>,
        ticket: Option<String>,
        at: impl Into<String>,
    ) -> Result<Self, crate::DomainError> {
        Ok(Self {
            actor: non_blank(actor, "activity_actor")?,
            kind: non_blank(kind, "activity_kind")?,
            summary: non_blank(summary, "activity_summary")?,
            ticket,
            at: non_blank(at, "activity_at")?,
        })
    }
}

/// The activity feed's terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ActivityFeedTerminalState {
    /// ≥1 activity entry.
    Succeeded {
        /// When the feed was read (RFC-3339 string).
        fetched_at: String,
        /// Who produced it (the state snapshot's activity array).
        source: String,
        /// The entries, newest first (the shipped slice order).
        entries: Vec<ActivityEntry>,
    },
    /// No activity yet — the state `core.js:780` already renders.
    Skipped {
        /// Why the feed is empty (always [`FailureReason::NoData`]).
        reason: FailureReason,
        /// The mandatory "what fills this panel" hint (the core's own —
        /// panels compose the core, they do not re-invent attribution).
        hint: FillHint,
    },
    /// The activity read failed.
    Failed {
        /// Non-blank human cause (core invariant).
        cause: String,
        /// Whether re-polling the snapshot may help.
        retryable: bool,
    },
}

impl ActivityFeedTerminalState {
    /// Terminal success: a non-empty entry set.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `source` is blank or the
    /// entry list is empty (that is the `Skipped` state, not a success).
    pub fn succeeded(
        fetched_at: String,
        source: impl Into<String>,
        entries: Vec<ActivityEntry>,
    ) -> Result<Self, crate::DomainError> {
        if entries.is_empty() {
            return Err(crate::DomainError::Empty {
                field: "activity_feed_entries",
            });
        }
        Ok(Self::Succeeded {
            fetched_at,
            source: non_blank(source, "activity_feed_source")?,
            entries,
        })
    }

    /// Terminal emptiness: nothing has happened yet, with attribution and
    /// the default hint.
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
            cause: non_blank(cause, "activity_feed_cause")?,
            retryable,
        })
    }
}

impl From<&ActivityFeedTerminalState> for TerminalState {
    fn from(panel: &ActivityFeedTerminalState) -> Self {
        match panel {
            ActivityFeedTerminalState::Succeeded {
                fetched_at, source, ..
            } => Self::Succeeded {
                fetched_at: fetched_at.clone(),
                source: source.clone(),
            },
            ActivityFeedTerminalState::Skipped { reason, hint } => Self::Skipped {
                reason: *reason,
                hint: hint.clone(),
            },
            ActivityFeedTerminalState::Failed { cause, retryable } => Self::Failed {
                cause: cause.clone(),
                retryable: *retryable,
            },
        }
    }
}

impl From<ActivityFeedTerminalState> for TerminalState {
    fn from(panel: ActivityFeedTerminalState) -> Self {
        Self::from(&panel)
    }
}

impl TryFrom<&TerminalState> for ActivityFeedTerminalState {
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

impl super::PanelTerminalState for ActivityFeedTerminalState {
    fn panel_id(&self) -> crate::overview_panel::OverviewPanelId {
        crate::overview_panel::OverviewPanelId::OvActivity
    }

    fn terminal_state(&self) -> TerminalState {
        TerminalState::from(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: u32) -> ActivityEntry {
        ActivityEntry::new(
            "DEV-FEATURE",
            "claimed",
            format!("claimed CXA-B{n}"),
            Some(format!("CXA-B{n}")),
            "2026-02-24T10:00:00Z",
        )
        .expect("actor/kind/summary/at are non-blank")
    }

    #[test]
    fn a_blank_summary_is_refused() {
        assert!(
            ActivityEntry::new("DEV-FEATURE", "claimed", " ", None, "2026-02-24T10:00:00Z")
                .is_err()
        );
    }

    #[test]
    fn an_empty_entry_list_is_not_a_success() {
        assert_eq!(
            ActivityFeedTerminalState::succeeded(
                "2026-02-24T10:00:00Z".to_owned(),
                "snapshot",
                vec![]
            ),
            Err(crate::DomainError::Empty {
                field: "activity_feed_entries"
            })
        );
    }

    #[test]
    fn skipped_is_no_data_with_the_default_hint() {
        let state = ActivityFeedTerminalState::skipped();
        match &state {
            ActivityFeedTerminalState::Skipped { reason, hint, .. } => {
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
        let panel = ActivityFeedTerminalState::succeeded(
            "2026-02-24T10:00:00Z".to_owned(),
            "GET /api/state",
            vec![entry(253)],
        )
        .expect("entries are non-empty");
        assert_eq!(
            TerminalState::from(&panel),
            TerminalState::Succeeded {
                fetched_at: "2026-02-24T10:00:00Z".to_owned(),
                source: "GET /api/state".to_owned(),
            }
        );
    }

    #[test]
    fn skipped_round_trips() {
        let panel = ActivityFeedTerminalState::skipped();
        let core = TerminalState::from(panel.clone());
        assert_eq!(ActivityFeedTerminalState::try_from(&core), Ok(panel));
    }

    #[test]
    fn a_cancelled_core_state_cannot_reconstruct_an_activity_feed() {
        assert!(ActivityFeedTerminalState::try_from(&TerminalState::Cancelled).is_err());
    }
}
