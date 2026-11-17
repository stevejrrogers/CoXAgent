//! `PaymentsPort` — the outbound boundary over a card-payment provider
//! (CXA-F451). PR-CI runs against a deterministic in-process stub; the nightly
//! job runs the same flows against the live sandbox through the HTTP adapter.
//! Any future billing feature MUST bind to this port rather than grow its own
//! HTTP client — that is the whole point of the seam (see CXA-F450 history).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// ISO-4217 alpha codes the surface supports today. New currencies are
/// additive — adapters must treat an unknown code as [`PaymentError::Invalid`]
/// rather than panicking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CurrencyCode {
    Usd,
    Eur,
    Gbp,
}

impl CurrencyCode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Usd => "USD",
            Self::Eur => "EUR",
            Self::Gbp => "GBP",
        }
    }
}

/// One charge request. `idempotency_key` is caller-chosen and STABLE across
/// retries of the same logical payment — adapters must de-duplicate on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChargeRequest {
    /// Smallest indivisible unit (cents), so no float money ever crosses the port.
    pub amount_minor: u64,
    pub currency: CurrencyCode,
    pub idempotency_key: String,
    /// Correlation to the caller's own aggregate, surfaced verbatim in errors.
    pub order_ref: String,
}

/// Terminal state of a successful charge call — success is still a state, not
/// just `Ok(_)`, because a decline is a *valid business outcome*, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChargeStatus {
    Charged,
    /// Card declined by the issuer; `reason` names WHY (attribution).
    Declined {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChargeReceipt {
    pub receipt_id: String,
    pub status: ChargeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundReceipt {
    pub refund_id: String,
    pub amount_minor: u64,
}

/// Every variant carries attribution data so a failure names WHO/WHY without
/// re-querying anything — matching the terminal-state convention.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaymentError {
    /// Provider unreachable/rate-limited/unhealthy. `status` is the last HTTP
    /// status (0 for transport-level failures); `retry_after` honours the
    /// vendor's hint when present.
    #[error("payment vendor unavailable (status={status}, retry_after_ms={retry_after_ms:?})")]
    VendorUnavailable {
        status: u16,
        retry_after_ms: Option<u64>,
    },
    /// Business refusal (decline outside the receipt model, unknown receipt,
    /// over-refund). `reason` is human-readable.
    #[error("payment refused: {0}")]
    Refused(String),
    /// Caller sent garbage — bad amount/currency pair. Fail before IO.
    #[error("invalid payment request (amount_minor={amount_minor}, currency={currency:?})")]
    Invalid {
        amount_minor: u64,
        currency: CurrencyCode,
    },
}

#[async_trait]
pub trait PaymentsPort: Send + Sync {
    /// Charge once. Idempotent per `req.idempotency_key`: retrying the same
    /// key must never double-charge.
    ///
    /// # Errors
    /// [`PaymentError`] with the variant matching the failure mode above.
    async fn charge(&self, req: ChargeRequest) -> Result<ChargeReceipt, PaymentError>;

    /// Refund part/all of a previously issued charge receipt.
    ///
    /// # Errors
    /// [`PaymentError::Refused`] for unknown receipts/over-refunds,
    /// [`PaymentError::VendorUnavailable`] on provider faults.
    async fn refund(
        &self,
        receipt_id: &str,
        amount_minor: u64,
    ) -> Result<RefundReceipt, PaymentError>;
}
