//! The `Goal` value object — a declared product goal line work is proposed
//! against (the PO's goal gate). Pure business model: no IO, no framework.
//!
//! The identity contract that the outcome ledger depends on lives here: a
//! goal's id is immutable and its title is editable wording. Anything that
//! references a goal (a ticket's declared association, a ledger entry) binds
//! to the id, so renaming the goal never rewrites history.

use crate::error::DomainError;
use crate::ids::GoalId;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Lifecycle of a declared goal line. `Retired` goals stop accepting NEW
/// associations (the gate closed) but keep every association and ledger entry
/// they already earned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Active,
    Retired,
}

impl fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Active => "active",
            Self::Retired => "retired",
        })
    }
}

/// A declared product goal: the stable line (`id`) plus its current wording
/// (`title`). Constructed validated — a goal without a name is not a goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,
    pub title: String,
    #[serde(default)]
    pub status: GoalStatus,
}

impl Goal {
    /// Construct a goal, rejecting a blank title.
    ///
    /// # Errors
    /// Returns [`DomainError::Empty`] when `title` is empty or whitespace-only.
    pub fn new(id: GoalId, title: impl Into<String>) -> Result<Self, DomainError> {
        let title = title.into();
        if title.trim().is_empty() {
            return Err(DomainError::Empty {
                field: "goal_title",
            });
        }
        Ok(Self {
            id,
            title: title.trim().to_owned(),
            status: GoalStatus::Active,
        })
    }

    /// Restate the goal's wording. The id — and therefore every recorded
    /// association — is untouched; only the string changes.
    ///
    /// # Errors
    /// Returns [`DomainError::Empty`] when the new title is blank.
    pub fn rename(&mut self, title: impl Into<String>) -> Result<(), DomainError> {
        let title = title.into();
        if title.trim().is_empty() {
            return Err(DomainError::Empty {
                field: "goal_title",
            });
        }
        title.trim().clone_into(&mut self.title);
        Ok(())
    }

    /// Close the gate: no new ticket may declare this goal, existing ones keep
    /// their attribution.
    pub fn retire(&mut self) {
        self.status = GoalStatus::Retired;
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == GoalStatus::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(title: &str) -> Goal {
        Goal::new(GoalId::new("G001").expect("id"), title).expect("goal")
    }

    #[test]
    fn blank_title_is_rejected() {
        assert!(Goal::new(GoalId::new("G001").expect("id"), "   ").is_err());
    }

    #[test]
    fn rename_changes_only_the_wording() {
        let mut g = goal("Faster merges");
        g.rename("Faster, safer merges").expect("rename");
        assert_eq!(g.id.as_str(), "G001");
        assert_eq!(g.title, "Faster, safer merges");
        assert!(g.rename("   ").is_err(), "blank is not a rename");
        assert_eq!(g.title, "Faster, safer merges", "failed rename is a no-op");
    }

    #[test]
    fn retire_closes_the_gate_but_keeps_the_line() {
        let mut g = goal("Faster merges");
        assert!(g.is_active());
        g.retire();
        assert_eq!(g.status, GoalStatus::Retired);
        assert_eq!(g.id.as_str(), "G001", "retiring never touches identity");
    }
}
