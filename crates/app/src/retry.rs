//! Retry-with-backoff for the one-shot connects the composition root performs
//! at boot (CXA-B114).
//!
//! A hub whose Postgres rejects it at boot used to make exactly one attempt
//! per connect and live with the result forever: projects parked in the broken
//! list, the audit sink and the system-chat KV silently downgraded. Both
//! drivers here take the schedule as a parameter and the schedules are pure
//! functions of the failure count, so the retry DECISION is unit-tested below
//! with a zero delay — no database, no real clocks.

use std::fmt::Display;
use std::future::Future;
use std::time::Duration;

/// A retry schedule: failure number (1-based) → delay before the next attempt.
pub(crate) type Backoff = fn(u32) -> Duration;

/// Boot-phase attempts per connect. Three tries covering ~6s of backoff
/// absorb a database that is still starting next to the app; anything slower
/// is the background recovery loop's job, so a hub with a permanently dead
/// store still finishes booting promptly.
pub(crate) const BOOT_ATTEMPTS: u32 = 3;

/// Boot-phase backoff: 2s doubling, capped at 8s.
pub(crate) fn boot_backoff(failure: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(failure).min(8))
}

/// Background-recovery backoff: 5s doubling, capped at 60s — a store that
/// recovers is picked up within a minute, for as long as the hub runs.
pub(crate) fn recovery_backoff(failure: u32) -> Duration {
    Duration::from_secs(
        5u64.saturating_mul(2u64.saturating_pow(failure.saturating_sub(1)))
            .min(60),
    )
}

/// Run `op` up to `attempts` times, sleeping `backoff(n)` after the n-th
/// failure. Returns the first success, or the last error once the attempts
/// are spent — the caller decides what a permanent failure means (a broken
/// label, a downgrade, a crash).
///
/// # Errors
/// The last error from `op` after all attempts failed.
pub(crate) async fn retrying<T, E, Fut, F>(
    what: &str,
    attempts: u32,
    backoff: Backoff,
    mut op: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: Display,
{
    let attempts = attempts.max(1);
    let mut attempt = 1;
    loop {
        // The wait is computed INSIDE the match and the sleep happens OUTSIDE
        // it: a boxed error (not Send) must never be alive across the backoff
        // await, or every spawned caller of this helper stops being Send.
        let wait = match op().await {
            Ok(value) => return Ok(value),
            Err(err) if attempt >= attempts => return Err(err),
            Err(err) => {
                tracing::warn!("{what}: attempt {attempt}/{attempts} failed: {err} — retrying");
                backoff(attempt)
            }
        };
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

/// Run `op` until it succeeds, sleeping `backoff(n)` after the n-th failure.
/// For connects the hub cannot afford to give up on (CXA-B114): the store may
/// be down for minutes or hours, and whatever `op` guards comes back to life
/// on the first attempt after it recovers.
pub(crate) async fn retrying_until_recovered<T, E, Fut, F>(
    what: &str,
    backoff: Backoff,
    mut op: F,
) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: Display,
{
    let mut attempt = 1u32;
    loop {
        // Same shape as `retrying`: the error dies with the match, the sleep
        // happens outside it, so the future stays Send for spawned callers.
        let wait = match op().await {
            Ok(value) => return value,
            Err(err) => {
                let delay = backoff(attempt);
                tracing::warn!("{what}: attempt {attempt} failed: {err} — next try in {delay:?}");
                delay
            }
        };
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn zero(_: u32) -> Duration {
        Duration::ZERO
    }

    #[test]
    fn boot_backoff_doubles_from_two_seconds_and_caps_at_eight() {
        assert_eq!(boot_backoff(1), Duration::from_secs(2));
        assert_eq!(boot_backoff(2), Duration::from_secs(4));
        assert_eq!(boot_backoff(3), Duration::from_secs(8));
        assert_eq!(boot_backoff(50), Duration::from_secs(8), "capped");
    }

    #[test]
    fn recovery_backoff_doubles_from_five_seconds_and_caps_at_a_minute() {
        let secs = |n: u64| Duration::from_secs(n);
        assert_eq!(recovery_backoff(1), secs(5));
        assert_eq!(recovery_backoff(2), secs(10));
        assert_eq!(recovery_backoff(3), secs(20));
        assert_eq!(recovery_backoff(4), secs(40));
        assert_eq!(recovery_backoff(5), secs(60));
        assert_eq!(recovery_backoff(500), secs(60), "capped");
    }

    #[tokio::test]
    async fn a_first_try_success_never_retries() {
        let calls = Cell::new(0);
        let result: Result<u32, &str> = retrying("probe", BOOT_ATTEMPTS, zero, || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move { Ok(n) }
        })
        .await;

        assert_eq!(result, Ok(1));
        assert_eq!(
            calls.get(),
            1,
            "a healthy connect is attempted exactly once"
        );
    }

    #[tokio::test]
    async fn a_transient_failure_is_absorbed_within_the_boot_window() {
        let calls = Cell::new(0);
        let result: Result<u32, &str> = retrying("probe", BOOT_ATTEMPTS, zero, || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move {
                if n < 3 {
                    Err("db error")
                } else {
                    Ok(n)
                }
            }
        })
        .await;

        assert_eq!(result, Ok(3));
        assert_eq!(calls.get(), 3, "two failures, then the third attempt wins");
    }

    #[tokio::test]
    async fn a_permanent_failure_returns_after_the_last_attempt() {
        let calls = Cell::new(0);
        let result: Result<u32, &str> = retrying("probe", 3, zero, || {
            calls.set(calls.get() + 1);
            async move { Err::<u32, _>("still down") }
        })
        .await;

        assert_eq!(result, Err("still down"));
        assert_eq!(calls.get(), 3, "every boot attempt is spent, not one");
    }

    #[tokio::test]
    async fn the_recovery_loop_keeps_trying_until_the_store_recovers() {
        let calls = Cell::new(0);
        let value: u32 = retrying_until_recovered("probe", zero, || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move {
                if n < 5 {
                    Err("db error")
                } else {
                    Ok(n)
                }
            }
        })
        .await;

        assert_eq!(value, 5);
        assert_eq!(
            calls.get(),
            5,
            "no attempt cap: recovery waits out the outage"
        );
    }
}
