//! Why a panel has no data, or why it failed — the validated payload value
//! objects a [`TerminalState`] variant carries (CXA-B252, part 1/3).
//!
//! Mirrors the B192 contract's payload shape: an `Empty` ships a fill hint
//! (the mandatory "what fills this panel" line, [`FillHint`]) and an error
//! ships a non-blank human cause plus its [`FailureReason`] classification.
//! A string payload without its reason is unrepresentable: the hint enters
//! through [`FillHint::for_reason`], the cause refuses blanks.
//!
//! Purity: std-only, zero IO; validation is a pure invariant check returning
//! `Result`.

/// The reason a panel reached its [`TerminalState::Skipped`] or
/// [`TerminalState::Cancelled`] state — why no work was done.
///
/// `#[non_exhaustive]` so new reasons are added deliberately, each with its
/// own default fill-hint semantics (the B192 [`FillHint`] pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FailureReason {
    /// The panel's data source answered successfully with no content.
    NoData,
    /// The viewer is not authorized for this panel's data.
    Forbidden,
    /// The data lives in an integration that is not connected.
    SourceNotConnected,
    /// A filter the viewer applied matched nothing.
    FilterMatchedNothing,
    /// The panel's content stream (websocket/SSE) never started.
    StreamNotStarted,
}

impl FailureReason {
    /// Every reason, in declared order — lets tests and callers enumerate the
    /// set without relying on variant order tricks.
    pub const ALL: &'static [Self] = &[
        Self::NoData,
        Self::Forbidden,
        Self::SourceNotConnected,
        Self::FilterMatchedNothing,
        Self::StreamNotStarted,
    ];

    /// The default human-readable cause for this reason.
    #[must_use]
    pub const fn cause(self) -> &'static str {
        match self {
            Self::NoData => "the source answered successfully with no content",
            Self::Forbidden => "you lack permission for this data",
            Self::SourceNotConnected => "the source integration is not connected",
            Self::FilterMatchedNothing => "no data matches the active filter",
            Self::StreamNotStarted => "the live stream is not running",
        }
    }
}

impl std::fmt::Display for FailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.cause())
    }
}

/// The mandatory "what fills this panel" hint a [`TerminalState::Skipped`]
/// state carries.
///
/// Construct through [`FillHint::new`] (refuses blanks) or
/// [`FillHint::for_reason`] (the reason's own default hint). A skipped state
/// without a usable hint is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FillHint(String);

impl FillHint {
    /// Construct a hint, refusing blank input.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `raw` is blank.
    pub fn new(raw: impl Into<String>) -> Result<Self, crate::DomainError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(crate::DomainError::Empty {
                field: "terminal_state_fill_hint",
            });
        }
        Ok(Self(raw))
    }

    /// The reason's own default hint, for callers without specialized copy —
    /// the B192 [`FillHint`] default semantics.
    #[must_use]
    pub fn for_reason(reason: FailureReason) -> Self {
        Self(match reason {
            FailureReason::NoData => {
                "Nothing here yet — create the first item to fill this panel.".to_owned()
            }
            FailureReason::Forbidden => {
                "You lack permission for this data — ask an admin for access.".to_owned()
            }
            FailureReason::SourceNotConnected => {
                "Connect the source integration to fill this panel.".to_owned()
            }
            FailureReason::FilterMatchedNothing => {
                "No matches — widen or clear the filter to see items.".to_owned()
            }
            FailureReason::StreamNotStarted => {
                "The live stream is not running — restart it to fill this panel.".to_owned()
            }
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FillHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_cites_a_cause_and_names_a_distinct_default_hint() {
        let mut hints: Vec<String> = FailureReason::ALL
            .iter()
            .map(|reason| FillHint::for_reason(*reason).as_str().to_owned())
            .collect();
        let distinct = hints.len();
        hints.sort_unstable();
        hints.dedup();
        assert_eq!(hints.len(), distinct, "each reason ships its own hint");
        for reason in FailureReason::ALL {
            assert!(!reason.cause().trim().is_empty());
        }
    }

    #[test]
    fn a_blank_hint_is_refused() {
        assert_eq!(
            FillHint::new("   "),
            Err(crate::DomainError::Empty {
                field: "terminal_state_fill_hint"
            })
        );
    }
}
