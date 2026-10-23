//! `#ov-drain` — the clean-base drain banner's terminal-state value types
//! (CXA-B253, part 2/3; panel 1 of the CXA-B228 enumeration
//! [`OverviewPanelId::OvDrain`]).
//!
//! Evidence: the banner paints `drainBanner("ov-drain")`
//! (`crates/presentation/src/web/js/shell.js:750`, `core.js:797`) and its
//! only data-dependent content is the count of open PRs blocking the
//! clean-base drain (`shell.js:734` — `DRAIN_CACHE.n`). Types only — no
//! wiring: nothing renders these yet (that is part 3).
//!
//! Core-state projection (pure, documented once per panel): a drain banner is
//! either SHOWING the blocking PR count (`Succeeded`) or NOT RENDERED at all
//! (`Cancelled` — the non-refactor goal and fetch-failure paths both clear
//! the element). `Failed`, `TimedOut` and `Skipped` are unrepresentable here,
//! so the reverse conversion refuses them.

use crate::error::DomainError;
use crate::terminal_state::TerminalState;

/// How many open PRs block the clean-base drain (`shell.js:734`).
///
/// A count cannot be negative, so the newtype refuses negatives at
/// construction — the same parse-don't-validate pattern as
/// [`crate::ids::TicketId`].
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GreenPrCount(u32);

impl GreenPrCount {
    /// The PR count reported by the `/prs` sweep.
    #[must_use]
    pub const fn new(count: u32) -> Self {
        Self(count)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for GreenPrCount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The drain banner's terminal state: the core state from CXA-B252 plus the
/// panel's own payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DrainBannerTerminalState {
    /// The banner is rendering, with the blocking PR count it shows.
    Succeeded {
        /// When the PR count was fetched (RFC-3339 string; part 3 owns
        /// timestamps).
        fetched_at: String,
        /// Who produced the count (the `/prs` sweep endpoint).
        source: String,
        /// The blocking PR count the banner displays.
        blocking_prs: GreenPrCount,
    },
    /// The banner is not rendered (non-refactor goal clears the element).
    Cancelled,
}

impl DrainBannerTerminalState {
    /// Terminal success with the blocking PR count.
    ///
    /// # Errors
    /// Returns [`DomainError::Empty`] when `source` is blank (the core
    /// attribution invariant).
    pub fn succeeded(
        fetched_at: String,
        source: impl Into<String>,
        blocking_prs: GreenPrCount,
    ) -> Result<Self, DomainError> {
        Ok(Self::Succeeded {
            fetched_at,
            source: non_blank(source, "drain_banner_source")?,
            blocking_prs,
        })
    }

    /// Terminal: the banner is not rendered.
    #[must_use]
    pub const fn cancelled() -> Self {
        Self::Cancelled
    }
}

impl From<&DrainBannerTerminalState> for TerminalState {
    fn from(panel: &DrainBannerTerminalState) -> Self {
        match panel {
            DrainBannerTerminalState::Succeeded {
                fetched_at, source, ..
            } => Self::Succeeded {
                fetched_at: fetched_at.clone(),
                source: source.clone(),
            },
            DrainBannerTerminalState::Cancelled => Self::Cancelled,
        }
    }
}

impl From<DrainBannerTerminalState> for TerminalState {
    fn from(panel: DrainBannerTerminalState) -> Self {
        Self::from(&panel)
    }
}

impl TryFrom<&TerminalState> for DrainBannerTerminalState {
    type Error = crate::DomainError;

    fn try_from(core: &TerminalState) -> Result<Self, Self::Error> {
        match core {
            TerminalState::Succeeded { fetched_at, source } => Ok(Self::Succeeded {
                fetched_at: fetched_at.clone(),
                source: source.clone(),
                blocking_prs: GreenPrCount::new(0),
            }),
            TerminalState::Cancelled => Ok(Self::Cancelled),
            // Succeeded core cannot rebuild the payload (documented
            // projection: it reconstructs at the zero point only), and
            // Failed/TimedOut/Skipped{other reason}/Cancelled-with-payload
            // are unrepresentable for this panel — the caller asks the
            // panel's own constructors instead.
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

impl super::PanelTerminalState for DrainBannerTerminalState {
    fn panel_id(&self) -> crate::overview_panel::OverviewPanelId {
        crate::overview_panel::OverviewPanelId::OvDrain
    }

    fn terminal_state(&self) -> TerminalState {
        TerminalState::from(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_blocking_count_is_the_empty_banner_state() {
        let state = DrainBannerTerminalState::succeeded(
            "2026-02-24T10:00:00Z".to_owned(),
            "GET /api/prs",
            GreenPrCount::new(0),
        )
        .expect("source is non-blank");
        match &state {
            DrainBannerTerminalState::Succeeded { blocking_prs, .. } => {
                assert_eq!(blocking_prs.get(), 0);
            }
            other => panic!("expected Succeeded, got {other:?}"),
        }
    }

    #[test]
    fn a_blank_source_is_refused() {
        assert_eq!(
            DrainBannerTerminalState::succeeded(
                "2026-02-24T10:00:00Z".to_owned(),
                "   ",
                GreenPrCount::new(3),
            ),
            Err(DomainError::Empty {
                field: "drain_banner_source"
            })
        );
    }

    #[test]
    fn panel_to_core_round_trips() {
        let panel = DrainBannerTerminalState::succeeded(
            "2026-02-24T10:00:00Z".to_owned(),
            "GET /api/prs",
            GreenPrCount::new(5),
        )
        .expect("source is non-blank");
        let core = TerminalState::from(&panel);
        assert_eq!(
            core,
            TerminalState::Succeeded {
                fetched_at: "2026-02-24T10:00:00Z".to_owned(),
                source: "GET /api/prs".to_owned(),
            }
        );
        // Core Succeeded carries no PR count: the inverse reconstructs at
        // the documented zero point.
        let back = DrainBannerTerminalState::try_from(&core).expect("Succeeded round-trips");
        assert_eq!(
            back,
            DrainBannerTerminalState::succeeded(
                "2026-02-24T10:00:00Z".to_owned(),
                "GET /api/prs",
                GreenPrCount::new(0),
            )
            .expect("source is non-blank")
        );
        assert_ne!(back, panel, "the count itself is panel-only payload");
    }

    #[test]
    fn cancelled_round_trips() {
        let panel = DrainBannerTerminalState::cancelled();
        let core = TerminalState::from(panel.clone());
        assert_eq!(core, TerminalState::Cancelled);
        assert_eq!(DrainBannerTerminalState::try_from(&core), Ok(panel));
    }

    #[test]
    fn a_failed_core_state_cannot_reconstruct_a_drain_banner() {
        let core = TerminalState::failed("the /prs fetch failed", true).expect("non-blank");
        assert!(DrainBannerTerminalState::try_from(&core).is_err());
    }
}
