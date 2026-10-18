//! Operator-invoked rollback to a prior known-good deploy (CXA-F011).
//!
//! The autonomous cycle auto-rolls back when a health gate fails for an inbound
//! deploy. But after a merge there was no human-invoked way to revert the live
//! app to a prior known-good version when a regression slips through post-merge.
//! This use case provides it — an operator control that runs on demand through
//! ports only. It is deliberately DISTINCT from CXA-F008's automated release
//! pipeline and from the in-cycle auto-rollback.

use crate::state::{HealthCheckResult, KnownGoodDeploy};

/// Why a manual rollback did not run — or what it rolled back to.
#[derive(Debug)]
pub enum RollbackOutcome {
    /// Executed and passed every gate including the post-deploy health check.
    Ok {
        target: String,
        summary: String,
        health_check: Option<HealthCheckResult>,
    },
    /// Executed but FAILED its own post-deploy health check — never reported as
    /// a clean rollback (AC#3).
    Failed {
        target: String,
        summary: String,
        health_check: Option<HealthCheckResult>,
    },
    /// A deploy or cycle is already running — racing it would corrupt both
    /// builds (AC#5); refused rather than interleaved.
    InFlight,
}

impl RollbackOutcome {
    #[must_use]
    pub fn ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    #[must_use]
    pub fn target(&self) -> Option<&str> {
        match self {
            Self::Ok { target, .. } | Self::Failed { target, .. } => Some(target),
            Self::InFlight => None,
        }
    }
}

/// Pure decision over [`crate::state::ProjectState`]: which known-good deploy an
/// operator rollback would target right now. Returns `None` when no deploy has
/// ever passed both gates (`last_good_deploy` unset) — there is nothing safe to
/// revert to and the control must be unavailable (AC#4) rather than attempt a
/// broken no-op revert.
#[must_use]
pub fn known_good_target(state: &crate::state::ProjectState) -> Option<KnownGoodDeploy> {
    state.last_good_deploy.clone()
}

/// Inputs an operator supplies for one manual rollback invocation.
pub struct ManualRollbackInput {
    /// Who invoked it — recorded verbatim in the audit trail (AC#5).
    pub operator: String,
}

impl ManualRollbackInput {
    #[must_use]
    pub fn new(operator: impl Into<String>) -> Self {
        Self {
            operator: operator.into(),
        }
    }
}
