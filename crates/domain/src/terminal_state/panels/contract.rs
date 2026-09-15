//! The shared contract every per-panel terminal state implements
//! (CXA-B253, part 2/3 of the CXA-B195b split of CXA-B229).
//!
//! Every per-panel type in [`super`] embeds the core
//! [`TerminalState`](crate::terminal_state::TerminalState) from CXA-B252 and
//! adds its own payload value objects. This trait is the one seam callers can
//! program against: it names the panel (a stable [`OverviewPanelId`] from the
//! CXA-B228 evidence-backed enumeration) and yields the embedded core state,
//! so shared presentation code can label and render any panel's terminal
//! state without knowing the per-panel payload.
//!
//! Conversions live OUTSIDE the trait as pure `From`/`TryFrom` impls on each
//! per-panel type (panel → core is lossless; core → panel reconstructs the
//! payload at its zero point — each panel file documents the projection).
//!
//! Purity: std-only, zero IO, no external crates.

use crate::overview_panel::OverviewPanelId;
use crate::terminal_state::TerminalState;

/// The per-panel terminal-state contract: a stable panel identity plus the
/// embedded core [`TerminalState`].
pub trait PanelTerminalState {
    /// Which panel this state belongs to — the stable id from the CXA-B228
    /// enumeration, never a display string.
    fn panel_id(&self) -> OverviewPanelId;

    /// The embedded core state, with the panel payload projected away — the
    /// shared vocabulary every consumer of the CXA-B252 contract speaks.
    fn terminal_state(&self) -> TerminalState;

    /// Consume into the core state. Provided for convenience: it must agree
    /// with [`Self::terminal_state`] (both go through the panel's pure
    /// `From<Self> for TerminalState` impl).
    fn into_terminal_state(self) -> TerminalState
    where
        Self: Sized + Into<TerminalState>,
    {
        self.into()
    }
}

#[cfg(test)]
mod conformance {
    use super::*;

    /// Compile-time conformance harness: every per-panel type must implement
    /// [`PanelTerminalState`]. A new panel type added to `panels/` without
    /// the trait fails HERE (single conformance test, CXA-B253 AC2).
    macro_rules! assert_implements_contract {
        ($($ty:ty),+ $(,)?) => {
            $(
                fn _conforms<S: PanelTerminalState>(state: &S) -> (OverviewPanelId, TerminalState) {
                    (state.panel_id(), state.terminal_state())
                }
                let _ = _conforms::<$ty>;
            )+
        };
    }

    #[test]
    fn every_per_panel_terminal_state_implements_the_contract() {
        use super::super::*;

        assert_implements_contract!(
            DrainBannerTerminalState,
            AlertStripTerminalState,
            WorkingAgentsTerminalState,
            KpiStripTerminalState,
            TeamHealthTerminalState,
            ActivityFeedTerminalState,
            ReleasesFeedTerminalState,
        );
    }

    /// The contract's two accessors must agree with the per-panel `From`
    /// conversion: the trait never diverges from the `Into` path.
    #[test]
    fn the_trait_accessors_agree_with_the_into_conversion() {
        use super::super::*;

        let panel = KpiStripTerminalState::cancelled();
        let via_trait = panel.terminal_state();
        let via_into: TerminalState = panel.into();
        assert_eq!(via_trait, via_into);
    }
}
