//! `#ov-changelog` — the releases feed's terminal-state value types
//! (CXA-B253, part 2/3; panel 7 of the CXA-B228 enumeration
//! [`OverviewPanelId::OvChangelog`]).
//!
//! Evidence: `core.js:789` paints the literal empty state
//! `'no releases yet'` and `core.js:816` paints the released-version list —
//! the empty state is a real, reachable state the shipped UI renders.
//! Types only — no wiring (that is part 3).
//!
//! Core-state projection: releases showing is `Succeeded`; "no releases
//! yet" is `Skipped` with [`FailureReason::NoData`] and the default fill
//! hint; `Failed` is the snapshot read failing.

use crate::DomainError;
use crate::terminal_state::{FailureReason, FillHint, TerminalState};

/// One released version as the feed renders it (`core.js:816`): the tag and
/// when it shipped — attribution for "when did this land" is the payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReleaseEntry {
    /// The released version tag (`v1.2.3`).
    pub tag: String,
    /// When the release shipped (RFC-3339 string; part 3 owns timestamps).
    pub released_at: String,
}

impl ReleaseEntry {
    /// Validate and construct one entry — both fields non-blank.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when either field is blank.
    pub fn new(
        tag: impl Into<String>,
        released_at: impl Into<String>,
    ) -> Result<Self, crate::DomainError> {
        Ok(Self {
            tag: non_blank(tag, "release_tag")?,
            released_at: non_blank(released_at, "release_released_at")?,
        })
    }
}

/// The releases feed's terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReleasesFeedTerminalState {
    /// ≥1 released version.
    Succeeded {
        /// When the feed was read (RFC-3339 string).
        fetched_at: String,
        /// Who produced it (the state snapshot's release history).
        source: String,
        /// The entries, newest first (the shipped reverse order).
        releases: Vec<ReleaseEntry>,
    },
    /// "No releases yet" — the state `core.js:789` already renders.
    Skipped {
        /// Why the feed is empty (always [`FailureReason::NoData`]).
        reason: FailureReason,
        /// The mandatory "what fills this panel" hint (the core's own —
        /// panels compose the core, they do not re-invent attribution).
        hint: FillHint,
    },
    /// The release-history read failed.
    Failed {
        /// Non-blank human cause (core invariant).
        cause: String,
        /// Whether re-polling the snapshot may help.
        retryable: bool,
    },
}

impl ReleasesFeedTerminalState {
    /// Terminal success: a non-empty release set.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `source` is blank or the
    /// release list is empty (that is the `Skipped` state, not a success).
    pub fn succeeded(
        fetched_at: String,
        source: impl Into<String>,
        releases: Vec<ReleaseEntry>,
    ) -> Result<Self, crate::DomainError> {
        if releases.is_empty() {
            return Err(crate::DomainError::Empty {
                field: "releases_feed_releases",
            });
        }
        Ok(Self::Succeeded {
            fetched_at,
            source: non_blank(source, "releases_feed_source")?,
            releases,
        })
    }

    /// Terminal emptiness: no releases yet, with attribution and the
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
            cause: non_blank(cause, "releases_feed_cause")?,
            retryable,
        })
    }
}

impl From<&ReleasesFeedTerminalState> for TerminalState {
    fn from(panel: &ReleasesFeedTerminalState) -> Self {
        match panel {
            ReleasesFeedTerminalState::Succeeded {
                fetched_at,
                source,
                ..
            } => Self::Succeeded {
                fetched_at: fetched_at.clone(),
                source: source.clone(),
            },
            ReleasesFeedTerminalState::Skipped { reason, hint } => Self::Skipped {
                reason: *reason,
                hint: hint.clone(),
            },
            ReleasesFeedTerminalState::Failed {
                cause,
                retryable,
            } => Self::Failed {
                cause: cause.clone(),
                retryable: *retryable,
            },
        }
    }
}

impl From<ReleasesFeedTerminalState> for TerminalState {
    fn from(panel: ReleasesFeedTerminalState) -> Self {
        Self::from(&panel)
    }
}

impl TryFrom<&TerminalState> for ReleasesFeedTerminalState {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn release(n: u32) -> ReleaseEntry {
        ReleaseEntry::new(format!("v0.{n}.0"), "2026-02-24T10:00:00Z")
            .expect("tag/released_at are non-blank")
    }

    #[test]
    fn a_blank_tag_is_refused() {
        assert!(ReleaseEntry::new(" ", "2026-02-24T10:00:00Z").is_err());
    }

    #[test]
    fn an_empty_release_list_is_not_a_success() {
        assert_eq!(
            ReleasesFeedTerminalState::succeeded(
                "2026-02-24T10:00:00Z".to_owned(),
                "snapshot",
                vec![],
            ),
            Err(crate::DomainError::Empty {
                field: "releases_feed_releases"
            })
        );
    }

    #[test]
    fn skipped_is_no_data_with_the_default_hint() {
        let state =
            ReleasesFeedTerminalState::skipped();
        match &state {
            ReleasesFeedTerminalState::Skipped { reason, hint, .. } => {
                assert_eq!(*reason, FailureReason::NoData);
                assert_eq!(hint.as_str(), FillHint::for_reason(FailureReason::NoData).as_str());
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn succeeded_to_core_carries_attribution() {
        let panel = ReleasesFeedTerminalState::succeeded(
            "2026-02-24T10:00:00Z".to_owned(),
            "GET /api/state",
            vec![release(4)],
        )
        .expect("releases are non-empty");
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
        let panel = ReleasesFeedTerminalState::skipped();
        let core = TerminalState::from(panel.clone());
        assert_eq!(ReleasesFeedTerminalState::try_from(&core), Ok(panel));
    }

    #[test]
    fn a_cancelled_core_state_cannot_reconstruct_a_releases_feed() {
        assert!(ReleasesFeedTerminalState::try_from(&TerminalState::Cancelled).is_err());
    }
}
