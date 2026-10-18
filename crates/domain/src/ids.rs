//! Newtype identifiers. Value objects — parse-don't-validate at construction.

use crate::error::DomainError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A ticket identifier such as `FEAT-001`, `BUG-042`, `CHORE-007`.
///
/// Stored as an opaque, non-empty string. Prefix conventions are enforced by
/// the ID minting logic, not by this type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TicketId(String);

impl TicketId {
    /// Construct a ticket id, rejecting blank input.
    ///
    /// # Errors
    /// Returns [`DomainError::Empty`] when `raw` is empty or whitespace-only.
    pub fn new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(DomainError::Empty { field: "ticket_id" });
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TicketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A declared product-goal identifier (`G001`, ...). Opaque, non-empty.
///
/// Goal associations (a ticket's declared goal, a ledger entry's goal) bind to
/// this id, never to the goal's title — so renaming a goal's wording never
/// severs what shipped work was attributed to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GoalId(String);

impl GoalId {
    /// Construct a goal id, rejecting blank input.
    ///
    /// # Errors
    /// Returns [`DomainError::Empty`] when `raw` is empty or whitespace-only.
    pub fn new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(DomainError::Empty { field: "goal_id" });
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GoalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier of a worker machine that executes jobs. Opaque, non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkerId(String);

impl WorkerId {
    /// # Errors
    /// Returns [`DomainError::Empty`] when `raw` is blank.
    pub fn new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(DomainError::Empty { field: "worker_id" });
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
