//! `EmailPort` — outbound transactional email (invites, password resets,
//! deploy alerts). Adapter-implemented; CXA-F444 ships the in-memory stub
//! (`StubMailer`); the real SMTP/vendor adapter is a follow-up that must pass
//! the same conformance suite.
//!
//! Contract guarantees every consumer flow relies on:
//! - a send either returns a receipt or a typed [`EmailError`] — it never
//!   hangs silently and never panics;
//! - `InvalidAddress` / `Rejected` describe *content* problems, while
//!   `TemporarilyUnavailable` describes a provider outage, so callers can
//!   distinguish "fix the data" from "retry later";
//! - everything handed to the adapter stays observable: implementers should
//!   also expose [`EmailOutboxPort`] so operators/QA can audit what left.

use async_trait::async_trait;

/// Maximum total length of an address (RFC 5321 forward-path limit).
const MAX_ADDRESS_LEN: usize = 254;
/// Maximum length of the local part before the `@`.
const MAX_LOCAL_PART_LEN: usize = 64;

/// A validated email address. Construct via [`TryFrom<&str>`] — the invariant
/// enforcement lives here, not at call sites (DDD: invalid states unrepresentable).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmailAddress(String);

impl TryFrom<&str> for EmailAddress {
    type Error = EmailError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let trimmed = raw.trim();
        let reject = |why: &str| EmailError::InvalidAddress(format!("'{raw}': {why}"));
        if trimmed.is_empty() {
            return Err(reject("empty address"));
        }
        if trimmed.len() > MAX_ADDRESS_LEN {
            return Err(reject("longer than 254 characters"));
        }
        let at_count = trimmed.matches('@').count();
        if at_count != 1 {
            return Err(reject(if at_count == 0 {
                "missing '@'"
            } else {
                "more than one '@'"
            }));
        }
        let (local, domain) = trimmed.split_once('@').unwrap_or_default();
        if local.is_empty() || local.contains(char::is_whitespace) {
            return Err(reject("local part empty or contains whitespace"));
        }
        if local.chars().count() > MAX_LOCAL_PART_LEN {
            return Err(reject("local part longer than 64 characters"));
        }
        if domain.is_empty() || domain.contains(char::is_whitespace) {
            return Err(reject("domain empty or contains whitespace"));
        }
        Ok(Self(trimmed.to_owned()))
    }
}

impl std::fmt::Display for EmailAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for EmailAddress {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl EmailAddress {
    /// Inner value for test fixtures that parse addresses once and reuse them.
    #[cfg(test)]
    #[expect(dead_code, reason = "fixture helper; consumed by port tests")]
    pub(crate) fn unchecked(value: String) -> Self {
        Self(value)
    }
}

/// One outbound email. Recipients are multi-address (bulk invite); the sender
/// identity comes from configuration at the composition root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessage {
    pub from: EmailAddress,
    pub to: Vec<EmailAddress>,
    pub subject: String,
    pub text_body: String,
}

impl EmailMessage {
    /// Guarded constructor: rejects an empty recipient list so "silently sent
    /// to nobody" can never exist.
    ///
    /// # Errors
    /// [`EmailError::Rejected`] when `to` is empty.
    pub fn new(
        from: EmailAddress,
        to: Vec<EmailAddress>,
        subject: impl Into<String>,
        text_body: impl Into<String>,
    ) -> Result<Self, EmailError> {
        if to.is_empty() {
            return Err(EmailError::Rejected("message has no recipients".to_owned()));
        }
        Ok(Self {
            from,
            to,
            subject: subject.into(),
            text_body: text_body.into(),
        })
    }
}

/// Provider acknowledgement for one accepted message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailReceipt {
    /// Opaque provider message id — unique per accepted send.
    pub message_id: String,
    /// RFC3339 UTC acceptance stamp (from the injected clock).
    pub accepted_at_utc: String,
}

/// Typed failure surface — no silent drops, every path tells the caller why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmailError {
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("message rejected: {0}")]
    Rejected(String),
    #[error("delivery temporarily unavailable")]
    TemporarilyUnavailable,
}

/// Driven port: hand one message to a mail provider.
#[async_trait]
pub trait EmailPort: Send + Sync {
    /// # Errors
    /// Typed [`EmailError`] — never swallowed inside the adapter.
    async fn send(&self, message: EmailMessage) -> Result<EmailReceipt, EmailError>;
}

/// Read-back port over what has been captured/sent, so a debug view (or QA)
/// can audit deliveries. The stub records in memory; a durable history belongs
/// to the future vendor integration. Ordered oldest-first, exactly as sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedEmail {
    pub message_id: String,
    pub to: Vec<String>,
    pub subject: String,
    pub text_body: String,
    pub accepted_at_utc: String,
}

#[async_trait]
pub trait EmailOutboxPort: Send + Sync {
    /// Every accepted message, in send order, with timestamps.
    fn outbox(&self) -> Vec<CapturedEmail>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> EmailAddress {
        EmailAddress::try_from(s).expect("valid test address")
    }

    #[test]
    fn valid_addresses_parse_and_roundtrip_display() {
        for ok in [
            "user@example.com",
            "first.last+tag@sub.domain.org",
            "ops@localhost",
        ] {
            assert_eq!(addr(ok).to_string(), ok);
            assert_eq!(addr(ok).as_ref(), ok);
        }
    }

    #[test]
    fn malformed_addresses_are_rejected_with_reason() {
        for bad in [
            "",
            "   ",
            "no-at-sign.example.com",
            "two@@at.com",
            "@nodomain.com",
            "nolocal@",
            "has space@example.com",
            "spaced @example.com",
        ] {
            match EmailAddress::try_from(bad) {
                Err(EmailError::InvalidAddress(msg)) => assert!(!msg.is_empty()),
                other => panic!("expected InvalidAddress for '{bad}', got {other:?}"),
            }
        }
    }

    #[test]
    fn length_caps_are_enforced() {
        let long_local = format!("{}@host.com", "a".repeat(65));
        let long_total = format!("{}@host.com", "a".repeat(255));
        assert!(matches!(
            EmailAddress::try_from(long_local.as_str()),
            Err(EmailError::InvalidAddress(_))
        ));
        // 250-char local + '@' + "host.com" (>254 total) must trip the cap…
        assert!(matches!(
            EmailAddress::try_from(long_total.as_str()),
            Err(EmailError::InvalidAddress(_))
        ));
        // …while a 250-char address (long domain, short local) passes.
        let under_limit = format!("a@{}", "b".repeat(248));
        assert_eq!(under_limit.len(), 250);
        assert!(EmailAddress::try_from(under_limit.as_str()).is_ok());
        // Exactly 254 chars is still fine (RFC 5321 forward-path limit).
        let at_cap = format!("a@{}", "b".repeat(252));
        assert_eq!(at_cap.len(), 254);
        assert!(EmailAddress::try_from(at_cap.as_str()).is_ok());
    }

    #[test]
    fn message_builder_rejects_no_recipients() {
        let from = addr("noreply@coxagent.dev");
        let err =
            EmailMessage::new(from.clone(), vec![], "hi", "body").expect_err("empty `to` rejected");
        assert!(matches!(err, EmailError::Rejected(_)));
        let ok = EmailMessage::new(from, vec![addr("a@b.c")], "hi", "body")
            .expect("single recipient fine");
        assert_eq!(ok.subject, "hi");
    }
}
