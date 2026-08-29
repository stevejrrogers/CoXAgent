//! `OutboxEntry` — one outbound alert spooled for durable delivery (CXA-F235).
//!
//! Pure value object: shape, limits and the retry-deadline arithmetic only.
//! Persistence lives behind [`crate::ports::outbound::OutboxStorePort`], the
//! HTTP drain in the infrastructure notifier adapter.

use serde::{Deserialize, Serialize};

/// A message longer than this is truncated at enqueue time — an alert is a
/// one-line summary, and the spool must stay bounded whatever a caller pastes.
pub const OUTBOX_MESSAGE_CAP_BYTES: usize = 4096;

/// Delivery attempts before an entry is marked dead and left for replay.
/// ~8 with the backoff schedule below spreads retries over a few hours.
pub const OUTBOX_MAX_ATTEMPTS: u8 = 8;

/// Delay before the first retry, in seconds. Doubles per failed attempt.
pub const OUTBOX_BASE_BACKOFF_SECS: i64 = 30;

/// Upper bound on any single backoff, so a long outage still re-probes the
/// webhook about once an hour instead of waiting unboundedly long.
pub const OUTBOX_MAX_BACKOFF_SECS: i64 = 3600;

/// Delivery lifecycle of one spooled alert.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxStatus {
    /// Awaiting (or mid-) delivery; also the status of an entry between retries.
    #[default]
    Pending,
    /// The webhook acknowledged it (2xx) — kept as acknowledgement history.
    Delivered,
    /// Every attempt failed; parked until an operator replays it.
    Dead,
}

/// One outbound alert in the durable spool. `id` doubles as the idempotency
/// key: it is assigned once at enqueue time and travels on every retry, so an
/// at-least-once receiver can deduplicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// Stable identity of the event across all its delivery attempts.
    #[serde(default)]
    pub id: u64,
    /// Machine-readable event kind (see `NotifyEvent::kind`) — additive.
    pub kind: String,
    /// The project the event belongs to.
    pub project: String,
    /// Human-readable one-line summary (capped at [`OUTBOX_MESSAGE_CAP_BYTES`]).
    pub message: String,
    #[serde(default)]
    pub status: OutboxStatus,
    /// Failed delivery attempts so far.
    #[serde(default)]
    pub attempts: u8,
    /// Unix seconds: earliest instant the entry may be claimed for its next
    /// attempt. `0` = immediately due. Doubles as the in-flight lease while a
    /// flusher is POSTing (claim bumps it forward by the lease TTL).
    #[serde(default)]
    pub next_attempt_at: i64,
    /// Unix seconds: when the event was spooled.
    #[serde(default)]
    pub created_at: i64,
}

impl OutboxEntry {
    /// A fresh, immediately-due entry from a `NotifyEvent`-shaped triple.
    /// `id` 0 means "assign at enqueue time" — the store owns identities.
    #[must_use]
    pub fn pending(kind: &str, project: &str, message: &str, now: i64) -> Self {
        Self {
            id: 0,
            kind: kind.to_owned(),
            project: project.to_owned(),
            message: clamp_message(message),
            status: OutboxStatus::Pending,
            attempts: 0,
            next_attempt_at: 0,
            created_at: now,
        }
    }

    /// Presentation status: `pending` entries that already failed at least once
    /// read as "failed, retrying" rather than a first send in flight.
    #[must_use]
    pub fn is_retrying(&self) -> bool {
        self.status == OutboxStatus::Pending && self.attempts > 0
    }
}

/// Truncate a message to [`OUTBOX_MESSAGE_CAP_BYTES`] without splitting a
/// UTF-8 code point.
#[must_use]
pub fn clamp_message(message: &str) -> String {
    if message.len() <= OUTBOX_MESSAGE_CAP_BYTES {
        return message.to_owned();
    }
    let mut end = OUTBOX_MESSAGE_CAP_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

/// Backoff before retry number `attempt` (1-based): base doubled per attempt,
/// clamped to [`OUTBOX_MAX_BACKOFF_SECS`].
#[must_use]
pub fn backoff_delay_secs(attempt: u8) -> i64 {
    let doubling = u32::from(attempt.saturating_sub(1)).min(20);
    OUTBOX_BASE_BACKOFF_SECS
        .saturating_mul(1_i64 << doubling)
        .min(OUTBOX_MAX_BACKOFF_SECS)
}

/// The state one failed attempt leaves the entry in: attempts increment, and
/// the entry either flips dead at [`OUTBOX_MAX_ATTEMPTS`] or schedules its
/// next attempt strictly after its previous deadline (`prev.next_attempt_at`),
/// so retries never pile onto the same instant no matter when the flusher runs.
#[must_use]
pub fn advance_after_failure(now: i64, prev: &OutboxEntry) -> OutboxEntry {
    let attempts = prev.attempts.saturating_add(1).min(OUTBOX_MAX_ATTEMPTS);
    let mut next = prev.clone();
    next.attempts = attempts;
    if attempts >= OUTBOX_MAX_ATTEMPTS {
        next.status = OutboxStatus::Dead;
        return next;
    }
    next.status = OutboxStatus::Pending;
    let delay = backoff_delay_secs(attempts);
    next.next_attempt_at = (now.saturating_add(delay)).max(prev.next_attempt_at.saturating_add(1));
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(attempts: u8, next_attempt_at: i64) -> OutboxEntry {
        OutboxEntry {
            id: 1,
            kind: "deploy_failed".to_owned(),
            project: "demo".to_owned(),
            message: "boom".to_owned(),
            status: OutboxStatus::Pending,
            attempts,
            next_attempt_at,
            created_at: 0,
        }
    }

    #[test]
    fn first_failure_schedules_one_base_delay_out() {
        // CXA-F235 test plan: next = now + base * 2^(attempts-1), clamped.
        let next = advance_after_failure(1_000, &entry(0, 0));
        assert_eq!(next.attempts, 1);
        assert_eq!(next.next_attempt_at, 1_000 + OUTBOX_BASE_BACKOFF_SECS);
    }

    #[test]
    fn backoff_doubles_and_clamps_at_the_cap() {
        assert_eq!(backoff_delay_secs(1), 30);
        assert_eq!(backoff_delay_secs(2), 60);
        assert_eq!(backoff_delay_secs(3), 120);
        assert_eq!(backoff_delay_secs(7), 1_920); // 30 * 2^6, still under the cap
        assert_eq!(backoff_delay_secs(8), OUTBOX_MAX_BACKOFF_SECS); // clamped
        assert_eq!(backoff_delay_secs(9), OUTBOX_MAX_BACKOFF_SECS);
    }

    #[test]
    fn exhausting_max_attempts_flips_dead() {
        // CXA-F235 test plan: the 8th failed attempt parks the entry dead.
        let mut e = entry(0, 0);
        for attempt in 1..=OUTBOX_MAX_ATTEMPTS {
            e = advance_after_failure(1_000, &e);
            if attempt < OUTBOX_MAX_ATTEMPTS {
                assert_eq!(e.status, OutboxStatus::Pending);
            }
        }
        assert_eq!(e.attempts, OUTBOX_MAX_ATTEMPTS);
        assert_eq!(e.status, OutboxStatus::Dead);
    }

    #[test]
    fn retry_deadlines_are_strictly_monotonic() {
        // CXA-F235 property: every transition raises the deadline, whatever
        // clock the flusher runs on — even one lagging behind the deadline.
        let mut e = entry(0, 0);
        let mut now = 5_000;
        let mut prev_deadline = 0;
        for _ in 0..OUTBOX_MAX_ATTEMPTS {
            e = advance_after_failure(now, &e);
            if e.status == OutboxStatus::Dead {
                break;
            }
            assert!(e.next_attempt_at > prev_deadline, "deadline must advance");
            prev_deadline = e.next_attempt_at;
            now = e.next_attempt_at - 10; // a lagging flusher clock
        }
        assert_eq!(e.status, OutboxStatus::Dead);
    }

    #[test]
    fn message_is_clamped_to_the_cap_on_a_char_boundary() {
        let long = "x".repeat(OUTBOX_MESSAGE_CAP_BYTES + 100);
        assert_eq!(clamp_message(&long).len(), OUTBOX_MESSAGE_CAP_BYTES);
        let multibyte = "é".repeat(OUTBOX_MESSAGE_CAP_BYTES); // 2 bytes each
        let clamped = clamp_message(&multibyte);
        assert!(clamped.len() <= OUTBOX_MESSAGE_CAP_BYTES);
        assert!(clamped.chars().all(|c| c == 'é'));
        assert_eq!(clamp_message("short"), "short");
    }

    #[test]
    fn pending_with_attempts_reads_as_retrying() {
        assert!(!entry(0, 0).is_retrying());
        assert!(entry(3, 0).is_retrying());
        let mut delivered = entry(3, 0);
        delivered.status = OutboxStatus::Delivered;
        assert!(!delivered.is_retrying());
    }
}
