//! Value objects shared across the domain model but owned by no single
//! aggregate: the ticket kind, priority, sizing, lifecycle status and team role.
//!
//! These are plain data with no dependencies of their own (only derives), which
//! is what lets every other module — the transition table, domain events and the
//! aggregate itself — depend on them without forming a cycle. Keeping them out of
//! the aggregate file (`ticket.rs`) also means [`DomainError`] can describe an
//! invalid transition using their types without importing the aggregate.
//!
//! [`DomainError`]: crate::error::DomainError

use serde::{Deserialize, Serialize};

/// The single ticket kind, discriminated by `type` (anti-Jira: one entity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketType {
    Feature,
    Bug,
    Chore,
}

/// Three-level priority — deliberately coarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Medium,
    High,
}

/// Coarse sizing used for the SA design gate (small can auto-pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    Small,
    Medium,
    Large,
}

/// Lifecycle status. Feature/chore and bug share `InProgress` and `Rejected`;
/// the transition table keeps the two lifecycles distinct per [`TicketType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    // Feature / chore lifecycle
    Pending,
    Ready,
    InProgress,
    Done,
    Documented,
    Rejected,
    /// Parked by a person (PO/SM/user): deliberately out of play — sprint
    /// auto-commit, refill and agent pickup all skip it — but NOT rejected:
    /// it resumes to `Pending` (feature/chore) or `Open` (bug) when unblocked.
    /// Built for work blocked on the outside world (a billing account, a
    /// vendor), which otherwise re-enters every sprint and starves DEV.
    OnHold,
    // Bug lifecycle
    Open,
    Fixed,
    Verified,
}

/// The nine team roles plus `User` (super-PO) and `System` (automated bookkeeping).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Ba,
    Po,
    Sm,
    Sa,
    Pd,
    DevBug,
    DevFeature,
    Test,
    Docs,
    User,
    System,
}
