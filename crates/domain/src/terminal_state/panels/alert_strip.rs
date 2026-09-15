//! `#ov-alerts` — the operator alert strip's terminal-state value types
//! (CXA-B253, part 2/3; panel 2 of the CXA-B228 enumeration
//! [`OverviewPanelId::OvAlerts`]).
//!
//! Evidence: `alertsHtml` paints the strip (`core.js:166–181`) from four
//! rule families (deploy health, budget burn, open bugs, reverted work); the
//! same file treats a populated-but-empty ledger as normal. Types only — no
//! wiring (that is part 3).
//!
//! Core-state projection: the strip showing ≥1 alert is `Succeeded`; a
//! zero-alert moment is a REAL, healthy state, so it maps to `Skipped` with
//! [`FailureReason::NoData`] and carries the domain's default fill hint
//! (attribution is shipped by [`AlertStripTerminalState::skipped`], not
//! invented here).

use crate::DomainError;
use crate::terminal_state::{FailureReason, FillHint, TerminalState};

/// One operator alert, as the strip renders it (`core.js:173–179`: a
/// severity color, a title, a detail line). Attribution travels WITH the
/// alert — `triggered_by`/`at` — because an alert strip that can't say why
/// it's red is a bare zero, not a terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OperatorAlert {
    /// Which rule family raised it (`deploy-failed`, `budget`,
    /// `open-bugs`, `reverted-work`).
    pub kind: String,
    /// Severity as the strip's CSS token (`red` or `amber`).
    pub severity: String,
    /// The alert title line.
    pub title: String,
    /// The human detail line beneath the title.
    pub detail: String,
    /// Who/what raised the alert (the rule or source system).
    pub triggered_by: String,
    /// When the alert was raised (RFC-3339 string; part 3 owns timestamps).
    pub at: String,
}

impl OperatorAlert {
    /// Validate and construct one alert — every field non-blank.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when any field is blank.
    pub fn new(
        kind: impl Into<String>,
        severity: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
        triggered_by: impl Into<String>,
        at: impl Into<String>,
    ) -> Result<Self, crate::DomainError> {
        Ok(Self {
            kind: non_blank(kind, "operator_alert_kind")?,
            severity: non_blank(severity, "operator_alert_severity")?,
            title: non_blank(title, "operator_alert_title")?,
            detail: non_blank(detail, "operator_alert_detail")?,
            triggered_by: non_blank(triggered_by, "operator_alert_triggered_by")?,
            at: non_blank(at, "operator_alert_at")?,
        })
    }
}

/// The alert strip's terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlertStripTerminalState {
    /// ≥1 operator alert, each with its full attribution.
    Succeeded {
        /// When the alert set was fetched (RFC-3339 string).
        fetched_at: String,
        /// Who produced the set (the state snapshot's alert rules).
        source: String,
        /// The alerts, in strip order. The slice is never empty here — that
        /// state is [`AlertStripTerminalState::Skipped`].
        alerts: Vec<OperatorAlert>,
    },
    /// No alerts right now — the healthy, not-failing state.
    Skipped {
        /// Why the strip is empty (always [`FailureReason::NoData`]).
        reason: FailureReason,
        /// The mandatory "what fills this panel" hint.
        hint: FillHint,    },
    /// The strip could not be evaluated.
    Failed {
        /// Non-blank human cause (core invariant).
        cause: String,
        /// Whether re-polling the state snapshot may help.
        retryable: bool,
    },
}

impl AlertStripTerminalState {
    /// Terminal success: a non-empty alert set.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `source` is blank or the
    /// alert list is empty (that is the `Skipped` state, not a success).
    pub fn succeeded(
        fetched_at: String,
        source: impl Into<String>,
        alerts: Vec<OperatorAlert>,
    ) -> Result<Self, crate::DomainError> {
        if alerts.is_empty() {
            return Err(crate::DomainError::Empty {
                field: "alert_strip_alerts",
            });
        }
        Ok(Self::Succeeded {
            fetched_at,
            source: non_blank(source, "alert_strip_source")?,
            alerts,
        })
    }

    /// Terminal emptiness: no alerts, with attribution and the default hint.
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
            cause: non_blank(cause, "alert_strip_cause")?,
            retryable,
        })
    }
}

impl From<&AlertStripTerminalState> for TerminalState {
    fn from(panel: &AlertStripTerminalState) -> Self {
        match panel {
            AlertStripTerminalState::Succeeded {
                fetched_at,
                source,
                ..
            } => Self::Succeeded {
                fetched_at: fetched_at.clone(),
                source: source.clone(),
            },
            AlertStripTerminalState::Skipped { reason, hint } => Self::Skipped {
                reason: *reason,
                hint: hint.clone(),
            },
            AlertStripTerminalState::Failed {
                cause,
                retryable,
            } => Self::Failed {
                cause: cause.clone(),
                retryable: *retryable,
            },
        }
    }
}

impl From<AlertStripTerminalState> for TerminalState {
    fn from(panel: AlertStripTerminalState) -> Self {
        Self::from(&panel)
    }
}

impl TryFrom<&TerminalState> for AlertStripTerminalState {
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

    fn alert(n: u32) -> OperatorAlert {
        OperatorAlert::new(
            "budget",
            "amber",
            "Budget nearly reached",
            "80% of $50",
            "spend-scan",
            "2026-02-24T10:00:00Z",
        )
        .unwrap_or_else(|_| alert_fallback(n))
    }

    fn alert_fallback(n: u32) -> OperatorAlert {
        OperatorAlert {
            kind: format!("budget-{n}"),
            severity: "amber".to_owned(),
            title: "Budget nearly reached".to_owned(),
            detail: "80% of $50".to_owned(),
            triggered_by: "spend-scan".to_owned(),
            at: "2026-02-24T10:00:00Z".to_owned(),
        }
    }

    #[test]
    fn an_empty_alert_list_is_not_a_success() {
        assert_eq!(
            AlertStripTerminalState::succeeded("2026-02-24T10:00:00Z".to_owned(), "snapshot", vec![]),
            Err(crate::DomainError::Empty {
                field: "alert_strip_alerts"
            })
        );
    }

    #[test]
    fn skipped_is_no_data_with_the_default_hint() {
        let state = AlertStripTerminalState::skipped();
        match &state {
            AlertStripTerminalState::Skipped { reason, hint, .. } => {
                assert_eq!(*reason, FailureReason::NoData);
                assert_eq!(hint.as_str(), FillHint::for_reason(FailureReason::NoData).as_str());
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn a_blank_cause_is_refused() {
        assert_eq!(
            AlertStripTerminalState::failed("  ", true, "2026-02-24T10:00:00Z".to_owned()),
            Err(crate::DomainError::Empty {
                field: "alert_strip_cause"
            })
        );
    }

    #[test]
    fn panel_to_core_round_trips_through_skipped_and_failed() {
        let skipped = AlertStripTerminalState::skipped();
        let core = TerminalState::from(skipped.clone());
        assert!(matches!(core, TerminalState::Skipped { .. }));
        assert_eq!(AlertStripTerminalState::try_from(&core), Ok(skipped));

        let failed = AlertStripTerminalState::failed(
            "the snapshot poll threw",
            true,
            "2026-02-24T10:01:00Z".to_owned(),
        )
        .expect("cause is non-blank");
        let core = TerminalState::from(failed.clone());
        assert!(matches!(core, TerminalState::Failed { .. }));
        assert_eq!(AlertStripTerminalState::try_from(&core), Ok(failed));
    }

    #[test]
    fn a_cancelled_core_state_cannot_reconstruct_an_alert_strip() {
        assert!(AlertStripTerminalState::try_from(&TerminalState::Cancelled).is_err());
    }
}
