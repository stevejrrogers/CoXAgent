//! `MergeLockPort` — a lease-based per-branch merge lock (CXA-F375a), so
//! concurrent runners serialize verify-and-merge per target branch instead of
//! racing full-cap sweeps into the same branch (the F375 stall class recorded
//! in `merge_policy.rs`).
//!
//! Lease semantics: every acquisition carries a TTL. A lease whose
//! `expires_at_unix` has passed is stealable by [`Self::try_acquire`], so a
//! crashed writer can never wedge merges permanently — recovery is the TTL,
//! no janitor. Attribution travels with the lease
//! ([`MergeLockHolder`]): who holds it, why, since when, until when.
//!
//! This is ephemeral coordination, deliberately NOT folded into
//! [`super::StateStorePort`], which owns durable ticket aggregates (ISP).

use crate::error::PortError;

/// Who holds a merge lease, and why — the attribution surface the sweep
/// report quotes when a merge is skipped (depth bar: a blocked STATE says WHO
/// changed it, WHEN, and WHY).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MergeLockHolder {
    /// Worker id (`account@host`) of the lease owner. Never a secret.
    pub holder: String,
    /// One-line reason the lease was taken (ticket, cycle, sweep).
    pub why: String,
    /// Unix seconds when the lease was taken.
    pub acquired_at_unix: i64,
    /// Unix seconds after which the lease is stealable. Server-side expiry
    /// enforces recovery even if the holder crashed before releasing.
    pub expires_at_unix: i64,
}

impl MergeLockHolder {
    /// True once this lease's expiry instant has passed (`expires_at_unix <
    /// now`): the lease is stealable. At exactly `now == expires_at_unix` the
    /// lease is still honoured — the last second belongs to the holder.
    #[must_use]
    pub fn expired_at(&self, now_unix: i64) -> bool {
        self.expires_at_unix < now_unix
    }
}

/// Try-lock outcome-free contract: acquire returns `Ok(false)` when the lock
/// is legitimately held (not an error), reserving [`PortError`] for real
/// backend failures so callers can distinguish "contended" from "coordination
/// is down".
#[async_trait::async_trait]
pub trait MergeLockPort: Send + Sync {
    /// Atomically try to take the lease on `key` for `holder` for `ttl_secs`.
    ///
    /// Succeeds (`Ok(true)`) when the key is free, already ours (re-granted /
    /// extended), or the incumbent lease's `expires_at_unix` has passed
    /// (stealable). Returns `Ok(false)` when a live foreign lease holds the
    /// key — contention, not failure. Exactly one of N concurrent acquirers
    /// must observe `Ok(true)` (optimistic CAS in the adapters).
    ///
    /// # Errors
    /// [`PortError`] on backend failure (callers must fail CLOSED — a merge
    /// never proceeds unguarded because coordination errored).
    async fn try_acquire(
        &self,
        key: &str,
        holder: &str,
        why: &str,
        ttl_secs: u64,
    ) -> Result<bool, PortError> {
        let _ = (key, holder, why, ttl_secs);
        Ok(true)
    }

    /// Release the lease on `key`. Owner-only: `Ok(true)` means THIS caller's
    /// lease was removed; `Ok(false)` means the caller is not the current
    /// holder (someone else took it, or it expired) and nothing was cleared —
    /// the honest inverse of [`Self::try_acquire`].
    ///
    /// # Errors
    /// [`PortError`] on backend failure.
    async fn release(&self, key: &str, holder: &str) -> Result<bool, PortError> {
        let _ = (key, holder);
        Ok(true)
    }

    /// The current lease holder, if any — live attribution for decisions and
    /// reports. May still be returned after its expiry instant (the reader
    /// decides with [`MergeLockHolder::expired_at`]).
    ///
    /// # Errors
    /// [`PortError`] on backend failure.
    async fn holder(&self, key: &str) -> Result<Option<MergeLockHolder>, PortError> {
        let _ = key;
        Ok(None)
    }
}

#[cfg(test)]
mod defaults_tests {
    //! Default impls keep single-runner behaviour byte-identical with zero
    //! wiring — always grant, renew/release always succeed, nobody holds
    //! (mirrors the `acquire_leader`/`claim_stage` defaults).

    use super::*;

    struct NoopLock;

    #[async_trait::async_trait]
    impl MergeLockPort for NoopLock {}

    #[tokio::test]
    async fn default_impl_always_grants_the_lease() {
        let lock = NoopLock;
        assert!(lock
            .try_acquire("merge-lock/develop", "dev@host", "cycle 1 sweep", 60)
            .await
            .expect("default acquire"));
    }

    #[tokio::test]
    async fn default_release_reports_success_and_default_holder_is_none() {
        let lock = NoopLock;
        assert!(lock.release("merge-lock/develop", "dev@host").await.expect("default release"));
        assert!(lock.holder("merge-lock/develop").await.expect("default holder").is_none());
    }

    #[test]
    fn expiry_boundary_is_inclusive_for_the_holder() {
        let held = MergeLockHolder {
            holder: "a@h".into(),
            why: "sweep".into(),
            acquired_at_unix: 100,
            expires_at_unix: 160,
        };
        assert!(!held.expired_at(159));
        assert!(!held.expired_at(160), "at == expires the last second is the holder's");
        assert!(held.expired_at(161));
    }
}
