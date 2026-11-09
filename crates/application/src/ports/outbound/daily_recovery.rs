//! Daily-recovery ports (CXA-F403 part 1/3) — signatures only. The part-2
//! cycle step implements these over `StateStorePort` (reader side: gather
//! [`RecoverySnapshot`]s from persisted state; writer side: persist the resume
//! plus its diagnosis line). No adapter ships in this subtask, so a missing
//! implementation is a compile-time fact for part 2, not a runtime one.

use crate::use_cases::daily_recovery::{RecoveryPlan, RecoveryVerdict};

/// Reads held-ticket candidates for the daily sweep.
///
/// Implementations gather plain [`crate::use_cases::daily_recovery::
/// RecoverySnapshot`] data out of persisted state (`tickets` status/type,
/// `ticket_redesigns` rescue counter, `attempt_failures`, `hold_reasons`,
/// parent liveness). Returning an empty vec is a valid result — nothing past
/// its budget today.
pub trait HeldTicketReader: Send + Sync {
    /// All held-bug candidates as pure snapshots, at the moment of the call.
    ///
    /// # Errors
    /// Implementation-defined persistence failure (store unavailable, decode
    /// error).
    fn held_candidates(
        &self,
    ) -> Result<Vec<crate::use_cases::daily_recovery::RecoverySnapshot>, String>;
}

/// Persists the sweep's outcome per resumed ticket: flips the ticket out of
/// hold and records the attribution/diagnosis line produced by
/// [`recovery_eligibility`](crate::use_cases::daily_recovery::recovery_eligibility).
pub trait TicketResumeWriter: Send + Sync {
    /// Persist one resume with its diagnosis line. Called once per eligible
    /// verdict; the implementation owns idempotency (a re-run must not
    /// double-resume).
    ///
    /// # Errors
    /// Implementation-defined persistence failure; the sweep must surface it,
    /// never swallow it.
    fn persist_resume(&self, verdict: &RecoveryVerdict, plan: &RecoveryPlan) -> Result<(), String>;
}
