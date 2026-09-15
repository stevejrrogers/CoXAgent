//! The five terminal states the B195 lineage established for an
//! above-the-fold Overview panel ([`TerminalState`]) — the pure core of the
//! CXA-B252 contract.
//!
//! Variant semantics are NOT invented here; each one is lifted from the
//! frozen B195 lineage (the shared component contract in presentation,
//! `crates/presentation/src/overview_panels.rs`) or the B192 contract
//! ([`crate::panel_terminal_state`]) it composes with:
//!
//! * `Succeeded` — the panel has data (the B192 `Loaded` outcome), with
//!   attribution: when it was fetched and who produced it;
//! * `Failed` — a fetch failure with a human cause and retryability (the B192
//!   `Error` outcome);
//! * `Cancelled` — a started attempt did not complete; the variant list is
//!   fixed by the frozen B195 component contract;
//! * `Skipped` — no attempt was made, with the mandatory reason and a
//!   validated "what fills this panel" hint (the B192 `Empty` outcome);
//! * `TimedOut` — the attempt exceeded its deadline, i.e. a non-retryable
//!   fetch failure in the B195 component contract.
//!
//! The enum is exhaustive ON PURPOSE: consumers must name every state, so a
//! variant added without a match arm is a compile error, not a silently
//! unlabelled panel. That guarantee, demonstrated by the doctest:
//!
//! ```compile_fail
//! use coxagent_domain::terminal_state::TerminalState;
//!
//! // `Skipped` has no arm — this must NOT compile (non-exhaustive patterns),
//! // and the same failure fires for any variant added without an arm.
//! fn label(state: &TerminalState) -> &'static str {
//!     match state {
//!         TerminalState::Succeeded { .. } => "succeeded",
//!         TerminalState::Failed { .. } => "failed",
//!         TerminalState::Cancelled => "cancelled",
//!         TerminalState::TimedOut { .. } => "timed out",
//!     }
//! }
//! ```
//!
//! Purity: std-only, zero IO, no external crates. The only allowed failure
//! channel is [`crate::DomainError`].

use super::failure::{FailureReason, FillHint};

/// The terminal state of an above-the-fold Overview panel.
///
/// Deliberately NOT `#[non_exhaustive]`: consumers must handle every state —
/// that is the point of the contract (the B192 [`crate::panel_terminal_state`]
/// convention). Serialization is deliberately deferred to part 3.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TerminalState {
    /// Terminal success: the panel's data, when it was fetched, and who
    /// produced it — attribution is part of the state, never optional.
    Succeeded {
        /// When the data was fetched (RFC-3339 string here; a parsed
        /// timestamp type is part 3's serialization concern).
        fetched_at: String,
        /// Who produced the data (endpoint, renderer, agent).
        source: String,
    },
    /// Terminal failure: a non-blank human cause, whether retrying can help.
    Failed {
        /// Why the fetch failed, validated non-blank.
        cause: String,
        /// Whether a retry may help.
        retryable: bool,
    },
    /// Terminal: a started attempt did not complete; no work was done.
    Cancelled,
    /// Terminal: no attempt was made, with the mandatory reason and a
    /// validated "what fills this panel" hint.
    Skipped {
        reason: FailureReason,
        hint: FillHint,
    },
    /// Terminal: the attempt exceeded its deadline — a non-retryable fetch
    /// failure in the B195 component contract.
    TimedOut { deadline: String },
}

impl TerminalState {
    /// Terminal success with validated attribution.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `source` is blank.
    pub fn succeeded(
        fetched_at: String,
        source: impl Into<String>,
    ) -> Result<Self, crate::DomainError> {
        Ok(Self::Succeeded {
            fetched_at,
            source: non_blank(source, "terminal_state_source")?,
        })
    }

    /// Terminal failure with a validated human cause.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `cause` is blank.
    pub fn failed(cause: impl Into<String>, retryable: bool) -> Result<Self, crate::DomainError> {
        Ok(Self::Failed {
            cause: non_blank(cause, "terminal_state_cause")?,
            retryable,
        })
    }

    /// Terminal: a started attempt did not complete; no work was done.
    #[must_use]
    pub const fn cancelled() -> Self {
        Self::Cancelled
    }

    /// Terminal: no attempt was made, with the reason's default hint.
    #[must_use]
    pub fn skipped(reason: FailureReason) -> Self {
        Self::Skipped {
            reason,
            hint: FillHint::for_reason(reason),
        }
    }

    /// Terminal: the attempt exceeded its deadline.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `deadline` is blank.
    pub fn timed_out(deadline: impl Into<String>) -> Result<Self, crate::DomainError> {
        Ok(Self::TimedOut {
            deadline: non_blank(deadline, "terminal_state_deadline")?,
        })
    }

    /// One-line human summary. Terminal states name themselves with their
    /// attribution/cause/hint — never the bare "loading…" string (the B192
    /// rule).
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Succeeded { source, .. } => format!("loaded — source: {source}"),
            Self::Failed { cause, retryable } => {
                if *retryable {
                    format!("error — {cause} (a retry may help)")
                } else {
                    format!("error — {cause} (retrying will not help)")
                }
            }
            Self::Cancelled => "cancelled — a started attempt did not complete".to_owned(),
            Self::Skipped { reason, hint } => format!("skipped ({reason}) — {hint}"),
            Self::TimedOut { deadline } => {
                format!("timed out — the attempt missed its {deadline} deadline")
            }
        }
    }

    /// Every terminal state is terminal (the module holds only terminal
    /// states; the B192 [`crate::panel_terminal_state::Phase`] owns the
    /// lifecycle that reaches them).
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        true
    }
}

/// Refuse blank payloads so an unattributable state is unrepresentable — the
/// B192 [`crate::panel_terminal_state::PanelSource`] pattern, shared by all
/// string payload invariants of this module.
fn non_blank(raw: impl Into<String>, field: &'static str) -> Result<String, crate::DomainError> {
    let raw = raw.into();
    if raw.trim().is_empty() {
        return Err(crate::DomainError::Empty { field });
    }
    Ok(raw)
}
