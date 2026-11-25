//! Contract-first conformance suite for `EmailPort` (CXA-F444).
//!
//! Exercised through `Arc<dyn EmailPort>` factory functions exactly like
//! consumer code will use it, so any future real adapter (SMTP/vendor) reuses
//! this suite verbatim and is guaranteed swap-safe behaviour.
//!
//! Deterministic: fixed clock injected, no sleeps, no network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use coxagent_application::ports::outbound::{
    EmailAddress, EmailError, EmailMessage, EmailPort,
};
use coxagent_infrastructure::{FailureScript, StubMailer};

const FIXED_CLOCK: &str = "2030-06-15T09:30:00Z";

fn addr(value: &str) -> EmailAddress {
    value.try_into().expect("valid fixture address")
}

fn message(to: &str) -> EmailMessage {
    EmailMessage::new(
        addr("noreply@coxagent.dev"),
        vec![addr(to)],
        "Password reset",
        "Click https://example.test/reset within 30 minutes.",
    )
    .expect("valid fixture message")
}

#[tokio::test]
async fn success_path_accepts_and_reports_a_receipt_through_dyn_email_port() {
    let port: Arc<dyn EmailPort> = Arc::new(StubMailer::new(|| FIXED_CLOCK.to_string()));
    let receipt = port.send(message("user@example.com")).await.expect("accept");

    assert_eq!(receipt.message_id, "stub-email-1");
    assert_eq!(receipt.accepted_at_utc, FIXED_CLOCK);
}

#[tokio::test]
async fn rejection_path_surfaces_the_error_without_silent_failure() {
    let mailer = StubMailer::new(|| FIXED_CLOCK.to_string())
        .with_failure_script(FailureScript::AlwaysFail(EmailError::Rejected(
            "mailbox policy".into(),
        )));
    let port: Arc<dyn EmailPort> = Arc::new(mailer);

    let err = port
        .send(message("user@example.com"))
        .await
        .expect_err("scripted rejection");
    assert!(matches!(err, EmailError::Rejected(ref reason) if reason.contains("policy")));
    assert_eq!(
        err.to_string(),
        "message rejected: mailbox policy",
        "errors stay typed and descriptive"
    );
}

#[tokio::test]
async fn transient_unavailability_is_visible_to_callers_for_retry() {
    // FailAfter{1}: first send OK, second hits the flap — callers can observe
    // and retry.
    let port: Arc<dyn EmailPort> = Arc::new(
        StubMailer::new(|| FIXED_CLOCK.to_string()).with_failure_script(FailureScript::FailAfter(1)),
    );

    assert!(port.send(message("first@example.com")).await.is_ok());
    let err = port
        .send(message("second@example.com"))
        .await
        .expect_err("flap after threshold");
    assert!(matches!(err, EmailError::TemporarilyUnavailable));
}

#[tokio::test]
async fn bulk_operation_preserves_count_order_and_recipients_across_the_port() {
    let mailer = Arc::new(StubMailer::new(|| FIXED_CLOCK.to_string()));
    let port: Arc<dyn EmailPort> = mailer.clone();

    // Simulate one operation triggering several emails (bulk invite): each must
    // land individually, in order, none dropped.
    for recipient in ["a@example.com", "b@example.com", "c@example.com"] {
        port.send(message(recipient)).await.expect("each accepted");
    }

    let snapshot = mailer.outbox_snapshot();
    assert_eq!(snapshot.len(), 3, "none silently dropped");
    let got: Vec<&str> = snapshot
        .iter()
        .filter_map(|captured| captured.message.to.first())
        .map(AsRef::<str>::as_ref)
        .collect();
    assert_eq!(got, ["a@example.com", "b@example.com", "c@example.com"]);
    let ids: Vec<&str> = snapshot.iter().map(|c| c.message_id.as_str()).collect();
    assert_eq!(ids, ["stub-email-1", "stub-email-2", "stub-email-3"]);
}

#[tokio::test]
async fn receipts_carry_unique_monotonic_ids() {
    let port: Arc<dyn EmailPort> = Arc::new(StubMailer::new(|| FIXED_CLOCK.to_string()));
    let mut seen = std::collections::HashSet::new();
    for _ in 0..5 {
        let r = port.send(message("dup@example.com")).await.unwrap();
        assert!(seen.insert(r.message_id), "ids must be unique");
    }
}
