//! `#kpis` — the KPI tile strip's terminal-state value types
//! (CXA-B253, part 2/3; panel 4 of the CXA-B228 enumeration
//! [`OverviewPanelId::Kpis`]).
//!
//! Evidence: `overviewKpis` paints the four tiles
//! (`crates/presentation/src/web/js/kpis.js:76`, mounted at `core.js:798`),
//! and `core.js:780` swaps the whole strip for the "Ready to build" empty
//! panel when neither tickets nor activity exist — so a zero-everything
//! moment is a REAL state the panel already renders. Types only — no wiring
//! (that is part 3).
//!
//! Core-state projection: tiles showing is `Succeeded`; the no-tickets-no-
//! activity state maps to `Skipped` with [`FailureReason::NoData`] and the
//! domain's default fill hint (a dedicated `Cancelled` would lose the fill
//! hint the depth bar requires); `Failed` is the snapshot fetch failing.

use crate::DomainError;
use crate::terminal_state::{FailureReason, FillHint, TerminalState};

/// One KPI tile: a label, its value, and the window the value was computed
/// over — a number without its window is the "bare 0" the depth bar forbids.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KpiTile {
    /// The tile label (`Open tickets`, `Merged today`…).
    pub label: String,
    /// The value as the tile renders it (counts and ratios pre-formatted —
    /// the strip is display-only).
    pub value: String,
    /// The window the value covers (`today`, `7d`, `lifetime`).
    pub window: String,
    /// The trend/delta line beneath the value (`+3 vs yesterday`), if the
    /// history to compute one exists.
    pub delta: Option<String>,
}

impl KpiTile {
    /// Validate and construct one tile — label, value and window must be
    /// non-blank.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when any of those fields is
    /// blank.
    pub fn new(
        label: impl Into<String>,
        value: impl Into<String>,
        window: impl Into<String>,
        delta: Option<String>,
    ) -> Result<Self, crate::DomainError> {
        Ok(Self {
            label: non_blank(label, "kpi_tile_label")?,
            value: non_blank(value, "kpi_tile_value")?,
            window: non_blank(window, "kpi_tile_window")?,
            delta,
        })
    }
}

/// The KPI tile strip's terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KpiStripTerminalState {
    /// The four tiles are showing, each with its window and delta.
    Succeeded {
        /// When the tiles were computed (RFC-3339 string).
        fetched_at: String,
        /// Who produced them (the state snapshot the tiles read).
        source: String,
        /// The tiles, in strip order. Never empty here — that state is
        /// [`KpiStripTerminalState::Skipped`].
        tiles: Vec<KpiTile>,
    },
    /// No tickets and no activity yet — the "Ready to build" state the
    /// shipped panel already renders (`core.js:780`).
    Skipped {
        /// Why the strip is empty (always [`FailureReason::NoData`]).
        reason: FailureReason,
        /// The mandatory "what fills this panel" hint (the core's own —
        /// panels compose the core, they do not re-invent attribution).
        hint: FillHint,
    },
    /// The snapshot read behind the tiles failed.
    Failed {
        /// Non-blank human cause (core invariant).
        cause: String,
        /// Whether re-polling the snapshot may help.
        retryable: bool,
    },
    /// Terminal: the strip is not rendered (e.g. the screen was torn down
    /// mid-cycle). Present so the panel type is total over the core enum.
    Cancelled,
}

impl KpiStripTerminalState {
    /// Terminal success: a non-empty tile set.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `source` is blank or the
    /// tile list is empty (that is the `Skipped` state, not a success).
    pub fn succeeded(
        fetched_at: String,
        source: impl Into<String>,
        tiles: Vec<KpiTile>,
    ) -> Result<Self, crate::DomainError> {
        if tiles.is_empty() {
            return Err(crate::DomainError::Empty {
                field: "kpi_strip_tiles",
            });
        }
        Ok(Self::Succeeded {
            fetched_at,
            source: non_blank(source, "kpi_strip_source")?,
            tiles,
        })
    }

    /// Terminal emptiness: nothing to measure yet, with attribution and the
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
            cause: non_blank(cause, "kpi_strip_cause")?,
            retryable,
        })
    }

    /// A `Cancelled` strip — the concrete state the conformance test needs a
    /// zero-construction constructor for. In production the strip never
    /// self-cancels (its empty state is `Skipped`), but the core enum has the
    /// variant, so the panel type carries it too.
    #[must_use]
    pub const fn cancelled() -> Self {
        Self::Cancelled
    }
}

impl From<&KpiStripTerminalState> for TerminalState {
    fn from(panel: &KpiStripTerminalState) -> Self {
        match panel {
            KpiStripTerminalState::Succeeded {
                fetched_at,
                source,
                ..
            } => Self::Succeeded {
                fetched_at: fetched_at.clone(),
                source: source.clone(),
            },
            KpiStripTerminalState::Skipped { reason, hint } => Self::Skipped {
                reason: *reason,
                hint: hint.clone(),
            },
            KpiStripTerminalState::Failed {
                cause,
                retryable,
            } => Self::Failed {
                cause: cause.clone(),
                retryable: *retryable,
            },
            KpiStripTerminalState::Cancelled => Self::Cancelled,
        }
    }
}

impl From<KpiStripTerminalState> for TerminalState {
    fn from(panel: KpiStripTerminalState) -> Self {
        Self::from(&panel)
    }
}

impl TryFrom<&TerminalState> for KpiStripTerminalState {
    type Error = crate::DomainError;

    fn try_from(core: &TerminalState) -> Result<Self, Self::Error> {
        match core {
            TerminalState::Skipped { reason, hint } if *reason == FailureReason::NoData => Ok(Self::Skipped { reason: *reason, hint: hint.clone() }),
            TerminalState::Failed { cause, retryable } => Ok(Self::Failed { cause: cause.clone(), retryable: *retryable }),
            TerminalState::Cancelled => Ok(Self::Cancelled),
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

    fn tile(n: u32) -> KpiTile {
        KpiTile::new(
            "Open tickets",
            format!("{n}"),
            "today",
            Some("+3 vs yesterday".to_owned()),
        )
        .expect("label/value/window are non-blank")
    }

    #[test]
    fn a_blank_tile_label_is_refused() {
        assert!(KpiTile::new("  ", "4", "today", None).is_err());
    }

    #[test]
    fn an_empty_tile_list_is_not_a_success() {
        assert_eq!(
            KpiStripTerminalState::succeeded("2026-02-24T10:00:00Z".to_owned(), "snapshot", vec![]),
            Err(crate::DomainError::Empty {
                field: "kpi_strip_tiles"
            })
        );
    }

    #[test]
    fn skipped_is_no_data_with_the_default_hint() {
        let state =
            KpiStripTerminalState::skipped();
        match &state {
            KpiStripTerminalState::Skipped { reason, hint, .. } => {
                assert_eq!(*reason, FailureReason::NoData);
                assert_eq!(hint.as_str(), FillHint::for_reason(FailureReason::NoData).as_str());
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn succeeded_to_core_carries_attribution_and_round_trips_at_zero() {
        let panel = KpiStripTerminalState::succeeded(
            "2026-02-24T10:00:00Z".to_owned(),
            "GET /api/state",
            vec![tile(4)],
        )
        .expect("tiles are non-empty");
        let core = TerminalState::from(&panel);
        assert_eq!(
            core,
            TerminalState::Succeeded {
                fetched_at: "2026-02-24T10:00:00Z".to_owned(),
                source: "GET /api/state".to_owned(),
            }
        );
        let back = KpiStripTerminalState::try_from(&core);
        assert!(back.is_err(), "Succeeded core cannot rebuild the payload — documented projection");
    }

    #[test]
    fn skipped_round_trips() {
        let panel = KpiStripTerminalState::skipped();
        let core = TerminalState::from(panel.clone());
        assert_eq!(KpiStripTerminalState::try_from(&core), Ok(panel));
    }

    #[test]
    fn cancelled_round_trips() {
        let panel = KpiStripTerminalState::cancelled();
        let core = TerminalState::from(panel.clone());
        assert_eq!(core, TerminalState::Cancelled);
        assert_eq!(KpiStripTerminalState::try_from(&core), Ok(panel));
    }
}
