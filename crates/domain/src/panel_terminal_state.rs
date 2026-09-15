//! Panel terminal-state contract (CXA-B192) — the reusable shell contract for
//! hub panels: skeleton → terminal (loaded/empty/error).
//!
//! This is the CONTRACT subtask of CXA-B192: value types plus a pure,
//! exhaustive transition function. No UI, no rendering, no wiring — the next
//! two subtasks consume this from presentation. Zero IO and zero framework
//! imports: the only third-party type is `time::OffsetDateTime` for the
//! attribution timestamps, the same choice `DomainEvent::at` makes. The clock
//! is a parameter of [`transition::transition`] (the caller reads it), so the
//! contract itself is a pure function of its arguments.
//!
//! Invariants, enforced here — by types where possible, by the transition
//! table where types alone cannot:
//!
//! 1. [`Phase::Skeleton`] is the only initial phase; `Loaded`/`Empty`/`Error`
//!    are terminal. Only `Retried` and `FetchStarted` may leave a terminal
//!    phase, and both go to Skeleton — so a regression to Skeleton is always
//!    an explicit, user-visible re-fetch, never an accident.
//! 2. `Loaded` requires `fetched_at` and `source`; `Empty` requires a `reason`
//!    and a non-blank fill hint; `Error` requires a non-blank `cause` and a
//!    `retryable` flag. Each is unrepresentable without its payload by
//!    construction: the fields live inside the enum variants (no default
//!    constructors), and the string payloads are newtypes whose `new` refuses
//!    blanks.
//! 3. [`transition::transition`] is exhaustive over phases × events: one
//!    match arm per cell of the transition table, compiler-enforced, so a
//!    future phase or event fails to compile here until it is classified.
//! 4. No phase value can render as a bare "loading…" string: Skeleton's
//!    summary names the initial state, and terminal phases name themselves
//!    with their attribution (guarded by
//!    `no_phase_renders_a_bare_loading_string`).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Why a panel has no data — the reason an `Empty` is empty.
///
/// `#[non_exhaustive]` so new reasons are added deliberately, each with its
/// own default fill-hint semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EmptyReason {
    /// The source answered successfully with no content.
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

impl EmptyReason {
    /// Default "what fills this panel" hint. Presentation may specialize the
    /// copy, but every `Empty` ships a hint even when the caller supplies
    /// none ([`FillHint::from_reason`]).
    #[must_use]
    pub fn fill_hint(self) -> &'static str {
        match self {
            Self::NoData => "Nothing here yet — create the first item to fill this panel.",
            Self::Forbidden => "You lack permission for this data — ask an admin for access.",
            Self::SourceNotConnected => "Connect the source integration to fill this panel.",
            Self::FilterMatchedNothing => "No matches — widen or clear the filter to see items.",
            Self::StreamNotStarted => {
                "The live stream is not running — restart it to fill this panel."
            }
        }
    }
}

/// The mandatory "what fills this panel" hint carried by `Phase::Empty`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FillHint(String);

impl FillHint {
    /// Construct a hint, refusing blank input so an `Empty` without a usable
    /// hint is unrepresentable.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `raw` is blank.
    pub fn new(raw: impl Into<String>) -> Result<Self, crate::DomainError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(crate::DomainError::Empty {
                field: "panel_fill_hint",
            });
        }
        Ok(Self(raw))
    }

    /// The reason's own default hint, for panels without specialized copy.
    #[must_use]
    pub fn from_reason(reason: EmptyReason) -> Self {
        Self(reason.fill_hint().to_owned())
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

/// Source attribution on a `Loaded` panel — who/what produced the data.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PanelSource(String);

impl PanelSource {
    /// Construct a source label, refusing blank input so a `Loaded` panel
    /// without attribution is unrepresentable.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `raw` is blank.
    pub fn new(raw: impl Into<String>) -> Result<Self, crate::DomainError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(crate::DomainError::Empty {
                field: "panel_source",
            });
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PanelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A human-readable panel failure cause. `Phase::Error` holds this directly —
/// never an `Option` — so an Error without a cause is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PanelErrorCause(String);

impl PanelErrorCause {
    /// Construct a cause, refusing blank input.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `raw` is blank.
    pub fn new(raw: impl Into<String>) -> Result<Self, crate::DomainError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(crate::DomainError::Empty {
                field: "panel_error_cause",
            });
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PanelErrorCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The panel lifecycle phase. `Skeleton` is the only initial phase;
/// `Loaded`/`Empty`/`Error` are terminal.
///
/// Deliberately NOT `#[non_exhaustive]`: consumers must handle every phase —
/// that is the point of the contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase<D> {
    /// Initial phase — the panel has no content yet and is fetching.
    Skeleton,
    /// Terminal success: the panel's data, when it was fetched, and who
    /// produced it.
    Loaded {
        data: D,
        fetched_at: OffsetDateTime,
        source: PanelSource,
    },
    /// Terminal emptiness: why, and what would fill the panel.
    Empty { reason: EmptyReason, hint: FillHint },
    /// Terminal failure: a human-readable cause, whether retrying can help,
    /// and when the fetch failed.
    Error {
        cause: PanelErrorCause,
        retryable: bool,
        fetched_at: OffsetDateTime,
    },
}

impl<D> Phase<D> {
    /// One-line human summary of the phase. Never the bare string
    /// "loading…" — a Skeleton says it is the initial skeleton, and terminal
    /// phases name themselves with their attribution/cause/hint.
    #[must_use]
    pub fn summary(&self) -> String
    where
        D: std::fmt::Display,
    {
        match self {
            Self::Skeleton => "skeleton — initial load in progress".to_owned(),
            Self::Loaded { source, .. } => format!("loaded — source: {source}"),
            Self::Empty { reason, hint } => format!("empty ({reason:?}) — {hint}"),
            Self::Error {
                cause, retryable, ..
            } => {
                if *retryable {
                    format!("error — {cause} (a retry may help)")
                } else {
                    format!("error — {cause} (retrying will not help)")
                }
            }
        }
    }

    /// True when the phase is one of the terminal phases.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Skeleton)
    }
}

/// Panel lifecycle events. `FetchSucceeded` carries the data AND the source
/// label: only the producer of the data knows who produced it, and a `Loaded`
/// phase without attribution must be unrepresentable — so the source enters
/// the contract here, at the same door as the data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PanelEvent<D> {
    /// A fetch began (initial or explicit re-fetch). From a terminal phase
    /// this is one of the two permitted regressions to Skeleton.
    FetchStarted,
    /// A fetch returned data, produced by `source`.
    FetchSucceeded { data: D, source: PanelSource },
    /// The source answered successfully but with nothing to show.
    DataBecameEmpty { reason: EmptyReason },
    /// A fetch failed.
    FetchFailed {
        cause: PanelErrorCause,
        retryable: bool,
    },
    /// The viewer explicitly asked to try again — the other permitted
    /// regression to Skeleton.
    Retried,
}

impl<D> PanelEvent<D> {
    /// [`PanelEvent::DataBecameEmpty`] with the given reason.
    #[must_use]
    pub fn became_empty(reason: EmptyReason) -> Self {
        Self::DataBecameEmpty { reason }
    }

    /// [`PanelEvent::FetchFailed`] with a validated cause.
    ///
    /// # Errors
    /// Returns [`crate::DomainError::Empty`] when `cause` is blank.
    pub fn failed(cause: impl Into<String>, retryable: bool) -> Result<Self, crate::DomainError> {
        Ok(Self::FetchFailed {
            cause: PanelErrorCause::new(cause)?,
            retryable,
        })
    }
}

/// The pure, exhaustive panel transition function: `(current, event, now) ->
/// next`. `now` is the wall-clock reading supplied by the caller (adapter or
/// shell), which keeps this function pure and deterministic.
///
/// Transition table — every cell is a real `match` arm below:
///
/// | from \ event    | FetchStarted | FetchSucceeded | DataBecameEmpty | FetchFailed | Retried |
/// |-----------------|--------------|----------------|-----------------|-------------|---------|
/// | Skeleton        | Skeleton     | Loaded         | Empty           | Error       | Skeleton|
/// | Loaded          | Skeleton     | Loaded(new)    | Empty           | Error       | Skeleton|
/// | Empty           | Skeleton     | Loaded         | Empty(reason)   | Error       | Skeleton|
/// | Error           | Skeleton     | Loaded         | Empty           | Error(new)  | Skeleton|
///
/// Forbidden by construction: nothing reaches Skeleton except via
/// `Retried`/`FetchStarted`, and no event maps a phase outside the five
/// columns above (the match is total, so the compiler rejects any future
/// event until it is classified here).
pub fn transition<D>(current: Phase<D>, event: PanelEvent<D>, now: OffsetDateTime) -> Phase<D> {
    match (current, event) {
        // ---- from Skeleton: the initial-load row -------------------------
        (Phase::Skeleton, PanelEvent::FetchStarted | PanelEvent::Retried) => Phase::Skeleton,
        (Phase::Skeleton, PanelEvent::FetchSucceeded { data, source }) => Phase::Loaded {
            data,
            fetched_at: now,
            source,
        },
        (Phase::Skeleton, PanelEvent::DataBecameEmpty { reason }) => Phase::Empty {
            reason,
            hint: FillHint::from_reason(reason),
        },
        (Phase::Skeleton, PanelEvent::FetchFailed { cause, retryable }) => Phase::Error {
            cause,
            retryable,
            fetched_at: now,
        },

        // ---- the ONLY two regressions to Skeleton ------------------------
        (
            _terminal @ (Phase::Loaded { .. } | Phase::Empty { .. } | Phase::Error { .. }),
            PanelEvent::FetchStarted | PanelEvent::Retried,
        ) => Phase::Skeleton,

        // ---- from any terminal phase: refresh in place, never regresses --
        (
            _terminal @ (Phase::Loaded { .. } | Phase::Empty { .. } | Phase::Error { .. }),
            PanelEvent::FetchSucceeded { data, source },
        ) => Phase::Loaded {
            data,
            fetched_at: now,
            source,
        },
        (
            _terminal @ (Phase::Loaded { .. } | Phase::Empty { .. } | Phase::Error { .. }),
            PanelEvent::DataBecameEmpty { reason },
        ) => Phase::Empty {
            reason,
            hint: FillHint::from_reason(reason),
        },
        (
            _terminal @ (Phase::Loaded { .. } | Phase::Empty { .. } | Phase::Error { .. }),
            PanelEvent::FetchFailed { cause, retryable },
        ) => Phase::Error {
            cause,
            retryable,
            fetched_at: now,
        },
    }
}
