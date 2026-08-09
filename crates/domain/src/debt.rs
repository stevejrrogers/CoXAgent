//! Types for the periodic tech-debt sweep.
//!
//! A [`DebtSignal`] carries one quantified, cycle-specific finding about the
//! codebase — such as a lint delta against a prior baseline — so a debt-sweep
//! ticket is filed from evidence derived by pure analysis rather than from
//! vague habit. Being plain data keeps these free of any IO and persistable
//! alongside `ProjectState`, where they make each sweep's findings auditable
//! across cycles.

use serde::{Deserialize, Serialize};

/// What kind of debt signal a sweep detected this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DebtSignalKind {
    /// The measured lint (clippy) error count rose above the prior baseline —
    /// someone added debt without paying it down in kind.
    LintRegression,
    /// Files that plausibly carry dead/unused symbols surfaced by scanning
    /// production sources for suspicious patterns — candidates reported for
    /// human judgment rather than filed as fact (a grep cannot prove absence).
    DeadCodeSuspects,
    /// Source modules shipped without an inner-doc (`//!`) header.
    MissingModuleDocs,
}

impl DebtSignalKind {
    /// A short human label used when rendering a signal into ticket text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LintRegression => "lint regression",
            Self::DeadCodeSuspects => "dead-code suspects",
            Self::MissingModuleDocs => "missing module docs",
        }
    }
}

/// One quantified debt finding from a single sweep cycle. Kept as data so state
/// can persist it and admit per-cycle auditing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebtSignal {
    pub kind: DebtSignalKind,
    /// Concrete magnitude behind the signal (e.g. count of lint errors).
    pub count: u64,
}

impl DebtSignal {
    /// A new signal of `kind` carrying `count`.
    #[must_use]
    pub fn new(kind: DebtSignalKind, count: u64) -> Self {
        Self { kind, count }
    }
}
