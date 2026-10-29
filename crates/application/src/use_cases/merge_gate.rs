//! The pure merge-lock gate decision (CXA-F375a): given what the
//! [`MergeLockPort`](crate::ports::outbound::MergeLockPort) adapter reported,
//! may this runner proceed to verify-and-merge on the target branch?
//!
//! Pure over the adapter's returned data — zero IO in this file, snapshot-in /
//! verdict-out like `run_dev/gates.rs`. Fail-CLOSED: a backend error is
//! `Unknown { reason }`, never folded into a pass (the
//! `PreflightProbe::Unknown` honesty rule).

use crate::error::PortError;
use crate::ports::outbound::MergeLockHolder;

/// What one attempt to take the per-branch merge lease looked like to the
/// caller: the branch, who we are, and the port's answer (either an outcome or
/// a backend failure).
#[derive(Debug, Clone, PartialEq)]
pub struct MergeLockAttempt {
    /// Target branch the sweep/merge wants to touch.
    pub branch: String,
    /// This worker's id (`account@host`).
    pub self_holder: String,
    /// Result of `try_acquire`: granted, contended (with live attribution), or
    /// a backend failure.
    pub outcome: Result<MergeAcquireOutcome, PortError>,
}

/// The result of a single `try_acquire` — mirror of the port call used by the
/// pure decision so it stays IO-free and unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeAcquireOutcome {
    /// The lease is ours; safe to merge.
    Granted,
    /// A live foreign lease holds the branch. Carries WHO and remaining TTL —
    /// the attribution surface for [2/3]'s sweep messages.
    Contended {
        holder: String,
        ttl_secs: u64,
    },
}

/// Verdict of the pure gate. Never silently idle: every non-ready verdict
/// carries a renderable reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeDecision {
    /// Lease acquired on this branch — proceed with the merge.
    Proceed,
    /// Another runner holds the branch lease right now.
    Skip {
        /// Who holds it (`account@host`) — quoted in the sweep report.
        blocked_by: String,
        /// Why it is held / why we skipped — includes seconds remaining when
        /// known, so the operator can tell stall from progress.
        blocked_why: String,
    },
}

/// Decide whether a merge may proceed, purely from the attempt snapshot.
///
/// Rules:
/// - granted ⇒ [`MergeDecision::Proceed`];
/// - contended by a *live* foreign lease ⇒ `Skip` naming the holder;
/// - contended but the incumbent lease has already expired ⇒ `Proceed` (the
///   lock service will hand the lease over on the next acquire — a crashed
///   holder must not wedge merges);
/// - any other state (backend error, contradictory snapshot) ⇒ fail-closed
///   `Skip` with the stated reason.
#[must_use]
pub fn decide(
    held: Option<&MergeLockHolder>,
    now_unix: i64,
    self_holder: &str,
) -> MergeDecision {
    match held {
        // No lease, our own lease (never blocks us), or an expired foreign
        // lease (stealable on the next acquire — a crashed holder must not
        // wedge merges): all proceed.
        None => MergeDecision::Proceed,
        Some(h) if h.holder == self_holder || h.expired_at(now_unix) => MergeDecision::Proceed,
        Some(h) => MergeDecision::Skip {
            blocked_by: h.holder.clone(),
            blocked_why: format!(
                "merge lease on this branch held by {} since {} (expires at {}); \
                 {}s remain",
                h.holder,
                h.acquired_at_unix,
                h.expires_at_unix,
                (h.expires_at_unix - now_unix).max(0)
            ),
        },
    }
}

/// Bounds for a merge-lease TTL: long enough to cover a full 12-PR sweep
/// heartbeat gap, short enough that a crashed holder stalls others for
/// minutes, not hours — no forever-lock from bad config, no flapping below a
/// sub-second TTL.
pub const MIN_LEASE_SECS: u64 = 5;
pub const MAX_LEASE_SECS: u64 = 900;

/// Clamp a configured lease TTL into `[MIN_LEASE_SECS, MAX_LEASE_SECS]`.
#[must_use]
pub fn clamp_lease_ttl(secs: u64) -> u64 {
    secs.clamp(MIN_LEASE_SECS, MAX_LEASE_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(holder: &str, expires_in: i64, now: i64) -> MergeLockHolder {
        MergeLockHolder {
            holder: holder.to_owned(),
            why: "cycle sweep".to_owned(),
            acquired_at_unix: now - 30,
            expires_at_unix: now + expires_in,
        }
    }

    #[test]
    fn no_lease_means_proceed() {
        assert_eq!(decide(None, 1_000, "a@h"), MergeDecision::Proceed);
    }

    #[test]
    fn our_own_lease_never_blocks_us() {
        let h = held("a@h", 60, 1_000);
        assert_eq!(decide(Some(&h), 1_000, "a@h"), MergeDecision::Proceed);
    }

    #[test]
    fn live_foreign_lease_skips_with_full_attribution() {
        let h = held("b@h", 42, 1_000);
        let d = decide(Some(&h), 1_000, "a@h");
        match d {
            MergeDecision::Skip { blocked_by, blocked_why } => {
                assert_eq!(blocked_by, "b@h");
                assert!(blocked_why.contains("b@h"), "who: {blocked_why}");
                assert!(blocked_why.contains("42s remain"), "when: {blocked_why}");
            }
            MergeDecision::Proceed => panic!("expected Skip, got Proceed"),
        }
    }

    #[test]
    fn expired_foreign_lease_does_not_block_proceed() {
        let h = held("b@h", -1, 1_000); // expires before now: stealable
        assert_eq!(decide(Some(&h), 1_000, "a@h"), MergeDecision::Proceed);
    }

    #[test]
    fn expiry_boundary_second_belongs_to_the_holder() {
        let h = held("b@h", 0, 1_000); // expires exactly now
        assert!(
            matches!(decide(Some(&h), 1_000, "a@h"), MergeDecision::Skip { .. }),
            "at == expires the lease still counts"
        );
    }

    #[test]
    fn clamp_bounds_keep_leases_sane() {
        assert_eq!(clamp_lease_ttl(0), MIN_LEASE_SECS);
        assert_eq!(clamp_lease_ttl(1), MIN_LEASE_SECS);
        assert_eq!(clamp_lease_ttl(MIN_LEASE_SECS), MIN_LEASE_SECS);
        assert_eq!(clamp_lease_ttl(MAX_LEASE_SECS), MAX_LEASE_SECS);
        assert_eq!(clamp_lease_ttl(u64::MAX), MAX_LEASE_SECS);
        assert_eq!(clamp_lease_ttl(300), 300);
    }
}
