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

impl TicketType {
    /// Stable snake_case key, matching the serde wire form — the key every
    /// aggregated map (metrics, ledgers) uses so dashboard code never
    /// re-derives it from `format!("{:?}", ..)`.
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            TicketType::Feature => "feature",
            TicketType::Bug => "bug",
            TicketType::Chore => "chore",
        }
    }
}

/// One discrete operator gate decision, as recorded by the human
/// governance-attention ledger (CXA-F230). One variant per human decision
/// surface, named for the endpoint event that produces it — the ledger maps
/// every recorded intervention back to exactly one of these, so aggregation
/// never has to parse action prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionKind {
    /// A person approved a designed ticket into `Ready` (`POST …/ready`).
    ReadyApprove,
    /// A person passed a fixed ticket at the verify gate (`POST …/verify`).
    VerifyPass,
    /// A person sent a fixed ticket back at the verify gate (`POST …/send-back`).
    VerifySendBack,
    /// A person approved a cost-held ticket to run (`POST …/approve-cost`).
    CostApprove,
    /// A person landed a PR the machine held for human eyes (`POST …/pr/:n/human`
    /// with `action=approve`).
    HumanPrReviewed,
    /// A person dismissed a PR's human-eyes hold (`…/pr/:n/human` with
    /// `action=dismiss`).
    HumanPrDismissed,
    /// A person pulled an auto-approval back to `Pending` (`POST …/undo-approval`).
    UndoAutoApprove,
}

impl InterventionKind {
    /// Stable snake_case wire key (the serde form), used as the map key in
    /// every aggregated attention row.
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            InterventionKind::ReadyApprove => "ready_approve",
            InterventionKind::VerifyPass => "verify_pass",
            InterventionKind::VerifySendBack => "verify_send_back",
            InterventionKind::CostApprove => "cost_approve",
            InterventionKind::HumanPrReviewed => "human_pr_reviewed",
            InterventionKind::HumanPrDismissed => "human_pr_dismissed",
            InterventionKind::UndoAutoApprove => "undo_auto_approve",
        }
    }

    /// Every kind, in the stable order aggregated rows are zero-filled in.
    pub const ALL: [InterventionKind; 7] = [
        InterventionKind::ReadyApprove,
        InterventionKind::VerifyPass,
        InterventionKind::VerifySendBack,
        InterventionKind::CostApprove,
        InterventionKind::HumanPrReviewed,
        InterventionKind::HumanPrDismissed,
        InterventionKind::UndoAutoApprove,
    ];
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
