//! Stub email adapter — network-free implementation of `EmailPort`.
//!
//! CXA-F444 ships the outbound email capability as a stubbed dependency so any
//! consumer flow (verification / reset-style notifications) can be validated
//! end-to-end without depending on a flaky vendor sandbox. Every message is
//! recorded in a process-local outbox that testers can inspect via
//! [`StubMailer::outbox_snapshot`]; the real SMTP/vendor adapter is a separate
//! follow-up that must satisfy the same conformance suite.
//!
//! Purely in-process IO only: no sockets, no secrets — offline by construction.

use std::sync::{Arc, Mutex};

use coxagent_application::ports::outbound::{EmailError, EmailMessage, EmailPort, EmailReceipt};
use tracing::info;

/// One captured send: everything needed to introspect what the stub "delivered".
#[derive(Debug, Clone)]
pub struct CapturedEmail {
    /// Monotonic, unique per mailer instance.
    pub message_id: String,
    /// RFC3339 UTC timestamp from the injected clock, at accept time.
    pub accepted_at_utc: String,
    /// The full message as it was handed to the port (recipients preserved).
    pub message: EmailMessage,
}

/// Scripted failure injection, for testing vendor-flap handling today.
#[derive(Debug, Clone, Default)]
pub enum FailureScript {
    /// Every send succeeds. (default)
    #[default]
    Never,
    /// Succeeds exactly `n` times, then reports
    /// [`EmailError::TemporarilyUnavailable`] forever after.
    FailAfter(u32),
    /// Always fails with the supplied error (cloned each call).
    AlwaysFail(EmailError),
}

impl FailureScript {
    fn outcome_for(&self, sent_so_far: u64) -> Result<(), EmailError> {
        match self {
            FailureScript::Never => Ok(()),
            // Sentinel-like: an impl-level counter, not part of the port data.
            FailureScript::AlwaysFail(err) => Err(err.clone()),
            FailureScript::FailAfter(n) if sent_so_far < u64::from(*n) => Ok(()),
            FailureScript::FailAfter(_) => Err(EmailError::TemporarilyUnavailable),
        }
    }
}

type SharedOutbox = Arc<Mutex<Vec<CapturedEmail>>>;

/// Deterministic, network-free [`EmailPort`] implementation.
///
/// - records every **accepted** message into a shared in-memory outbox;
/// - honours the injected [`FailureScript`] BEFORE capturing (failed sends are
///   never attributed as delivered);
/// - generates monotonic fake receipt ids (`stub-email-<n>`);
/// - logs each send at `tracing::info!`;
/// - timestamps come from a caller-injected fixed clock for exact assertions.
#[derive(Clone)]
pub struct StubMailer {
    inner: Arc<Inner>,
}

struct Inner {
    seq: Mutex<u64>,
    outbox: SharedOutbox,
    script: Mutex<FailureScript>,
    clock: Box<dyn Fn() -> String + Send + Sync>,
}

impl StubMailer {
    /// Create a mailer whose `clock` returns an RFC3339 UTC string; inject a
    /// constant for deterministic tests, a real one in composition wiring.
    pub fn new(clock: impl Fn() -> String + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                seq: Mutex::new(0),
                outbox: Arc::new(Mutex::new(Vec::new())),
                script: Mutex::new(FailureScript::Never),
                clock: Box::new(clock),
            }),
        }
    }

    /// Install a scripted-failure behaviour (builder style). Succeeds-then-fails
    /// and always-fail scripts let reliability goals be exercised offline.
    ///
    /// # Panics
    /// Only if an internal lock is poisoned by a concurrent panic — treated as
    /// unrecoverable for an in-process stub.
    #[must_use]
    pub fn with_failure_script(self, script: FailureScript) -> Self {
        *self.inner.script.lock().expect("failure-script lock") = script;
        self
    }

    /// Point-in-time copy of every accepted message, in send order. Failed
    /// sends are absent — the outbox is an attribution record, not a retry log.
    ///
    /// # Panics
    /// Only if an internal lock is poisoned by a concurrent panic — treated as
    /// unrecoverable for an in-process stub.
    #[must_use]
    pub fn outbox_snapshot(&self) -> Vec<CapturedEmail> {
        self.inner.outbox.lock().expect("outbox lock").clone()
    }

    fn next_message_id(&self) -> String {
        let mut seq = self.inner.seq.lock().expect("sequence lock");
        *seq += 1;
        format!("stub-email-{seq}")
    }
}

#[async_trait::async_trait]
impl EmailPort for StubMailer {
    async fn send(&self, message: EmailMessage) -> Result<EmailReceipt, EmailError> {
        // Resolve the script first so failures never reach the outbox.
        let sent = *self.inner.seq.lock().expect("sequence lock");
        {
            let script = self.inner.script.lock().expect("failure-script lock");
            script.outcome_for(sent)?;
        }

        let message_id = self.next_message_id();
        let accepted_at_utc = (self.inner.clock)();
        info!(
            message_id = %message_id,
            recipients = message.to.iter().map(AsRef::<str>::as_ref).collect::<Vec<_>>().join(","),
            subject = %message.subject,
            "email accepted (stub)"
        );
        self.inner
            .outbox
            .lock()
            .expect("outbox lock")
            .push(CapturedEmail {
                message_id: message_id.clone(),
                accepted_at_utc: accepted_at_utc.clone(),
                message,
            });
        Ok(EmailReceipt {
            message_id,
            accepted_at_utc,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_application::ports::outbound::EmailAddress;

    const FIXED_CLOCK: &str = "2031-04-01T12:00:00Z";

    fn addr(value: &str) -> EmailAddress {
        value.try_into().expect("valid fixture address")
    }

    fn message(to: &str) -> EmailMessage {
        EmailMessage::new(
            addr("noreply@coxagent.dev"),
            vec![addr(to)],
            "Reset your password",
            "Click https://example.test/reset",
        )
        .expect("valid fixture message")
    }

    #[tokio::test]
    async fn send_is_accepted_and_captured_with_fixed_clock_stamp() {
        let mailer = StubMailer::new(|| FIXED_CLOCK.to_string());
        let receipt = mailer.send(message("alice@example.com")).await.unwrap();

        assert_eq!(receipt.message_id, "stub-email-1");
        assert_eq!(receipt.accepted_at_utc, FIXED_CLOCK);

        let snapshot = mailer.outbox_snapshot();
        assert_eq!(snapshot.len(), 1);
        let captured = &snapshot[0];
        assert_eq!(captured.message_id, receipt.message_id);
        assert_eq!(captured.accepted_at_utc, FIXED_CLOCK);
        assert_eq!(captured.message.to, vec![addr("alice@example.com")]);
    }

    #[tokio::test]
    async fn multiple_recipient_sends_are_all_captured_in_order() {
        let mailer = StubMailer::new(|| FIXED_CLOCK.to_string());
        for recipient in ["one@example.com", "two@example.com", "three@example.com"] {
            mailer.send(message(recipient)).await.unwrap();
        }

        let snapshot = mailer.outbox_snapshot();
        assert_eq!(snapshot.len(), 3, "no silently dropped emails");
        let got: Vec<&str> = snapshot
            .iter()
            .filter_map(|c| c.message.to.first())
            .map(AsRef::<str>::as_ref)
            .collect();
        assert_eq!(
            got,
            ["one@example.com", "two@example.com", "three@example.com"]
        );
        let ids: Vec<&str> = snapshot.iter().map(|c| c.message_id.as_str()).collect();
        assert_eq!(ids, ["stub-email-1", "stub-email-2", "stub-email-3"]);
    }

    #[tokio::test]
    async fn failed_sends_never_appear_in_the_outbox() {
        let mailer = StubMailer::new(|| FIXED_CLOCK.to_string()).with_failure_script(
            FailureScript::AlwaysFail(EmailError::Rejected("policy".into())),
        );

        assert!(mailer.send(message("bob@example.com")).await.is_err());
        assert!(
            mailer.outbox_snapshot().is_empty(),
            "failures must not be attributed as delivered"
        );

        // A fresh Never-scripted mailer still works — the failure is scripted,
        // not ambient.
        let healthy = StubMailer::new(|| FIXED_CLOCK.to_string());
        healthy.send(message("bob@example.com")).await.unwrap();
        assert_eq!(healthy.outbox_snapshot().len(), 1);
    }

    #[tokio::test]
    async fn fail_after_yields_temporary_unavailability_then_recovers_by_rebuilding() {
        let mailer = StubMailer::new(|| FIXED_CLOCK.to_string())
            .with_failure_script(FailureScript::FailAfter(2));

        for _ in 0..2 {
            assert!(mailer.send(message("ok@example.com")).await.is_ok());
        }
        let err = mailer
            .send(message("late@example.com"))
            .await
            .expect_err("third send should hit the flap");
        assert!(matches!(err, EmailError::TemporarilyUnavailable));
        assert_eq!(mailer.outbox_snapshot().len(), 2);
    }
}
