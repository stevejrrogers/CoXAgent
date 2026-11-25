//! Mock payment-retry harness (CXA-F450) — a deterministic, offline stand-in
//! for the card-payment provider sandbox so QA can exercise every retry path
//! without burning shared sandbox credentials (team lesson: parallelised
//! integration tests hammering one sandbox erode pipeline trust).
//!
//! Design: PURE core over injected data. Zero IO — the caller supplies each
//! request's timestamp and consumer id, so runs are fully reproducible in CI
//! and the module passes the hexagonal gate unchanged. Attempts are recorded
//! in an append-only, mutex-ordered log (AC4), keyed per attempt index.
//!
//! Scenarios (the sprint decision locked these four paths):
//! - [`ScenarioKind::RetryableDecline`] (AC1): first response declines with a
//!   retryable failure code; a retry inside the configured `retry_window`
//!   (attempts remaining before the window closes) approves.
//! - [`ScenarioKind::PermanentFailure`] (AC2): every attempt returns the SAME
//!   failure code; no success is ever returned during the session.
//!
//! Both scenarios respect a configurable `max_retries` limit (AC3): once it is
//!   spent, the harness answers with the explicit
//!   [`PaymentStatus::RetriesExhausted`] signal instead of responding normally.
//!
//! # Concurrency

use serde::Serialize;
use std::sync::Mutex;

/// Terminal outcome of a mocked authorization attempt (approve / decline /
/// exhausted), named verbatim from AC4's outcome vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    /// Authorization succeeded.
    Approved,
    /// Authorization was refused; see the paired failure code.
    Declined,
    /// The scenario's max-retry limit `N` is spent — nothing responds
    /// normally any more until the harness is rebuilt (explicit AC3 signal).
    RetriesExhausted,
}

/// Selectable scripted behaviour of this harness session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioKind {
    /// AC1: decline once, approve retries made while the retry window is open.
    RetryableDecline {
        /// How many attempts AFTER the initial decline may still approve
        /// (the configured retry window, in attempts — deterministic here,
        /// unlike wall-clock windows, so CI cannot flake on scheduling).
        retry_window: usize,
    },
    /// AC2: identical refusal on every attempt, forever.
    PermanentFailure {
        /// Stable, comparable code returned verbatim on EVERY attempt.
        failure_code: &'static str,
    },
}

impl ScenarioKind {
    /// Which scenario rule produced a given attempt (AC4 attribution).
    #[must_use]
    pub fn rule_name(&self) -> &'static str {
        match self {
            Self::RetryableDecline { .. } => "retryable_decline",
            Self::PermanentFailure { .. } => "permanent_failure",
        }
    }
}

/// A session's immutable configuration: scenario + retry ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioConfig {
    pub kind: ScenarioKind,
    /// Configurable max-retry limit N (AC3): attempts beyond this get the
    /// explicit exhausted signal instead of a normal response.
    pub max_retries: usize,
}

/// One entry of the ordered attempt log (AC4): who called, when (caller-
/// supplied stamp), what came back, and which rule produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptRecord {
    /// Position of this attempt within the scenario (0-based, monotonic).
    pub attempt_index: usize,
    /// Which consumer sent the attempt (concurrent consumers stay separated).
    pub consumer_id: String,
    /// Timestamp string supplied by the caller's clock (kept as text so the
    /// core stays pure and deterministic).
    pub timestamp: String,
    /// approve / decline / exhausted.
    pub outcome: PaymentStatus,
    /// Which scenario rule produced this outcome.
    pub rule: &'static str,
    /// Machine-comparable failure code, present iff the attempt was refused.
    pub failure_code: Option<String>,
}

/// What a mocked gateway call hands back: status plus its failure code (if
/// the attempt did not authorize).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessResponse {
    pub status: PaymentStatus,
    pub failure_code: Option<String>,
    /// Attempt index this response consumed — lets callers correlate the
    /// response with the attempt-log entry it produced.
    pub attempt_index: usize,
}

/// The harness itself: scenario config + an ordered, lock-guarded attempt log
/// shared safely across concurrent consumers (AC4).
#[derive(Debug)]
pub struct MockPaymentHarness {
    config: ScenarioConfig,
    /// Append-only log; the Mutex guarantees interleaved consumers land
    /// strictly ordered by attempt_index.
    log: Mutex<Vec<AttemptRecord>>,
}

/// Refusal code a RetryableDecline emits for its deliberate first decline
/// (distinct from `window_closed` so callers can tell "retry me" apart from
/// "too late").
pub const RETRYABLE_CODE: &str = "retryable_decline";

impl MockPaymentHarness {
    /// Start a harness session running `config`.
    #[must_use]
    pub fn new(config: ScenarioConfig) -> Self {
        Self {
            config,
            log: Mutex::new(Vec::new()),
        }
    }

    /// Convenience constructor for the AC1 'retryable decline' scenario.
    #[must_use]
    pub fn retryable_decline(retry_window: usize, max_retries: usize) -> Self {
        Self::new(ScenarioConfig {
            kind: ScenarioKind::RetryableDecline { retry_window },
            max_retries,
        })
    }

    /// Convenience constructor for the AC2 'permanent failure' scenario.
    #[must_use]
    pub fn permanent_failure(failure_code: &'static str, max_retries: usize) -> Self {
        Self::new(ScenarioConfig {
            kind: ScenarioKind::PermanentFailure { failure_code },
            max_retries,
        })
    }

    /// Send one mocked authorization attempt. `timestamp` is caller-supplied
    /// (injectable clock) keeping this function free of IO.
    ///
    /// Decision table (in order):
    /// Decision table (in order):
    ///
    /// 1. attempts already past `max_retries` ->
    ///    [`PaymentStatus::RetriesExhausted`]; otherwise,
    /// 2. [`ScenarioKind::PermanentFailure`] responds with the same code every
    ///    time; otherwise,
    /// 3. [`ScenarioKind::RetryableDecline`] declines attempt 0, approves
    ///    while the retry window is open, refuses permanently past it.
    ///
    /// # Panics
    pub fn authorize(&self, consumer_id: &str, timestamp: &str) -> HarnessResponse {
        let attempt_index = {
            // A panic while holding the lock cannot happen here (no user code
            // runs under it), but do not poison-spread if it ever did.
            let mut log = self
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let idx = log.len();
            let (status, code) = self.decide(idx);
            log.push(AttemptRecord {
                attempt_index: idx,
                consumer_id: consumer_id.to_owned(),
                timestamp: timestamp.to_owned(),
                outcome: status,
                rule: self.config.kind.rule_name(),
                failure_code: code.clone(),
            });
            (idx, status, code)
        };
        let (idx, status, code) = attempt_index;
        HarnessResponse {
            status,
            failure_code: code,
            attempt_index: idx,
        }
    }

    /// PURE decision over the attempt index — everything above is plumbing.
    fn decide(&self, attempt_index: usize) -> (PaymentStatus, Option<String>) {
        // AC3: the explicit exhausted signal once the configured limit is gone.
        if attempt_index > self.config.max_retries {
            return (PaymentStatus::RetriesExhausted, None);
        }
        match &self.config.kind {
            // AC2: one stable code on every attempt, never an approval.
            ScenarioKind::PermanentFailure { failure_code } => {
                (PaymentStatus::Declined, Some((*failure_code).to_owned()))
            }
            // AC1: decline first, approve retries inside the window; past the
            // window the decline stops being retryable (stable hard-refusal
            // code, distinct from the transient one).
            ScenarioKind::RetryableDecline { retry_window } => {
                if attempt_index == 0 || attempt_index > *retry_window {
                    let code = if attempt_index == 0 {
                        crate::mock_payment_harness::RETRYABLE_CODE
                    } else {
                        "window_closed"
                    };
                    (PaymentStatus::Declined, Some(code.to_owned()))
                } else {
                    (PaymentStatus::Approved, None)
                }
            }
        }
    }

    /// Read-only snapshot of the attempt log, strictly in send order.
    ///
    /// # Errors
    /// Never in practice; surfaced because the log lives behind a lock whose
    /// poisoning we do not swallow silently.
    pub fn attempts(&self) -> Result<Vec<AttemptRecord>, String> {
        self.log
            .lock()
            .map(|l| l.clone())
            .map_err(|_| "attempt log lock poisoned".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(n: u8) -> String {
        format!("2026-11-18T10:00:{n:02}Z")
    }

    /// AC1: first response declines, a retry within the window approves.
    #[test]
    fn ac1_retryable_decline_declines_then_approves_inside_the_window() {
        let h = MockPaymentHarness::retryable_decline(3, 5);
        let first = h.authorize("pos-1", &stamp(0));
        assert_eq!(first.status, PaymentStatus::Declined);
        assert_eq!(
            first.failure_code.as_deref(),
            Some("retryable_decline"),
            "first refusal carries the retryable code"
        );
        let retry = h.authorize("pos-1", &stamp(5));
        assert_eq!(
            retry.status,
            PaymentStatus::Approved,
            "retry approves inside window"
        );

        // Attempt log (AC4) records every leg with attribution.
        let log = h.attempts().expect("log");
        assert_eq!(log.len(), 2);
        assert_eq!(
            (log[0].outcome, log[0].rule),
            (PaymentStatus::Declined, "retryable_decline")
        );
        assert_eq!(
            (
                log[1].outcome,
                log[1].consumer_id.as_str(),
                log[1].timestamp.as_str(),
            ),
            (PaymentStatus::Approved, "pos-1", stamp(5).as_str())
        );
    }

    /// AC1 edge: a retry AFTER the window closed is refused with a distinct,
    /// stable `window_closed` code — not silently re-approved or retried.
    #[test]
    fn ac1_a_retry_after_the_window_closes_is_refused_distinctly() {
        let h = MockPaymentHarness::retryable_decline(1, 9);
        assert_eq!(h.authorize("c", &stamp(0)).status, PaymentStatus::Declined);
        assert_eq!(h.authorize("c", &stamp(1)).status, PaymentStatus::Approved);
        let late = h.authorize("c", &stamp(2));
        assert_eq!(
            late.failure_code.as_deref(),
            Some("window_closed"),
            "late attempts get the explicit too-late refusal"
        );
    }

    /// AC2: permanent failure returns the same failure code on every attempt
    /// and never approves; attribution names the permanent_failure rule.
    #[test]
    fn ac2_permanent_failure_repeats_one_stable_code_and_never_approves() {
        let h = MockPaymentHarness::permanent_failure("card_invalid", 4);
        for i in 0..4_u8 {
            let r = h.authorize("consumer-a", &stamp(i));
            assert_eq!(r.status, PaymentStatus::Declined);
            assert_eq!(r.failure_code.as_deref(), Some("card_invalid"));
        }
        let log = h.attempts().expect("log");
        assert!(log.iter().all(|a| a.rule == "permanent_failure"));
    }

    /// AC3: once max_retries N is spent the harness signals
    /// RetriesExhausted instead of answering normally ever again.
    #[test]
    fn ac3_when_the_retry_limit_is_spent_the_response_signals_exhausted() {
        let h = MockPaymentHarness::retryable_decline(5, 2);
        // attempt indexes 0,1,2 answer within the configured limit...
        for i in 0..3_u8 {
            assert_ne!(
                h.authorize("c", &stamp(i)).status,
                PaymentStatus::RetriesExhausted,
                "attempt {i} is still inside the limit"
            );
        }
        // ...from index 3 (> max_retries=2) on, the explicit signal fires.
        let past = h.authorize("c", &stamp(3));
        assert_eq!(past.status, PaymentStatus::RetriesExhausted);
        assert!(past.failure_code.is_none());
        assert_eq!(
            h.authorize("c", &stamp(4)).status,
            PaymentStatus::RetriesExhausted,
            "exhausted stays exhausted"
        );
        let log = h.attempts().expect("log");
        assert_eq!(log[3].outcome, PaymentStatus::RetriesExhausted);
    }

    /// AC4: concurrent consumers interleave into ONE ordered log keyed by
    /// consumer id, with monotonic per-consumer attempt attribution.
    #[test]
    fn ac4_concurrent_consumers_are_recorded_in_one_ordered_log() {
        use std::sync::Arc;
        use std::thread;

        let h = Arc::new(MockPaymentHarness::retryable_decline(3, 10));
        let handles: Vec<_> = (0..4)
            .map(|w| {
                let h = Arc::clone(&h);
                thread::spawn(move || {
                    for _ in 0..25 {
                        h.authorize(&format!("worker-{w}"), &stamp(w));
                    }
                })
            })
            .collect();
        for hd in handles {
            hd.join().expect("worker thread");
        }
        let log = h.attempts().expect("log");
        assert_eq!(log.len(), 100, "every authorize lands exactly once");
        // Strictly monotonic global ordering.
        for w in log.windows(2) {
            assert!(w[0].attempt_index < w[1].attempt_index);
        }
        // 25 recorded attempts per consumer, each attributed correctly.
        for w in 0..4 {
            let mine: Vec<_> = log
                .iter()
                .filter(|a| a.consumer_id == format!("worker-{w}"))
                .collect();
            assert_eq!(mine.len(), 25);
            assert!(mine.iter().all(|a| a.rule == "retryable_decline"));
        }
    }
}
