//! Deterministic in-process stub adapter (CXA-F451) — the PR-CI half of the
//! payments split. Root cause it kills: shared sandbox credentials hammered by
//! parallelised tests with no backoff. Every test instance owns its own
//! scripted faults and state; nothing is shared, so no cross-test flake.

use coxagent_application::ports::outbound::{
    ChargeReceipt, ChargeRequest, ChargeStatus, CurrencyCode, PaymentError, PaymentsPort,
    RefundReceipt,
};

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// One scripted fault the stub emits before falling through to success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptedFault {
    /// Emit `VendorUnavailable` once (optionally honouring a retry hint).
    VendorUnavailable {
        status: u16,
        retry_after_ms: Option<u64>,
    },
    /// Decline the charge with an issuer reason.
    Declined { reason: String },
}

/// Per-key fault sequences + seen idempotency keys, all instance-local.
#[derive(Default)]
struct StubState {
    faults: Mutex<HashMap<String, Vec<ScriptedFault>>>,
    charges: Mutex<HashMap<String, ChargeReceipt>>,
    refunds: Mutex<HashMap<String, Vec<u64>>>,
    counter: AtomicU64,
}

/// The stub adapter. Construct per-suite via [`StubPayments::new`] — never
/// share one between suites that script different faults.
pub struct StubPayments {
    state: StubState,
}

impl StubPayments {
    /// Build a stub pre-loaded with faults keyed by idempotency key. An empty
    /// map means every charge succeeds on first attempt.
    #[must_use]
    pub fn new(scripted_faults: HashMap<String, Vec<ScriptedFault>>) -> Self {
        Self {
            state: StubState {
                faults: Mutex::new(scripted_faults),
                ..StubState::default()
            },
        }
    }

    fn next_receipt_id(&self) -> String {
        let n = self.state.counter.fetch_add(1, Ordering::Relaxed) + 1;
        format!("stub_rcpt_{n}")
    }

    fn take_scripted(&self, key: &str) -> Option<PaymentError> {
        let mut faults = self
            .state
            .faults
            .lock()
            .map_err(|_| PaymentError::Refused("stub state poisoned".into()))
            .ok()?;
        match faults.get_mut(key)?.pop() {
            Some(ScriptedFault::VendorUnavailable {
                status,
                retry_after_ms,
            }) => Some(PaymentError::VendorUnavailable {
                status,
                retry_after_ms,
            }),
            Some(ScriptedFault::Declined { reason }) => Some(PaymentError::Refused(reason)),
            None => None,
        }
    }
}

fn validate(req: &ChargeRequest) -> Result<(), PaymentError> {
    if req.amount_minor == 0 || req.idempotency_key.is_empty() {
        return Err(PaymentError::Invalid {
            amount_minor: req.amount_minor,
            currency: req.currency,
        });
    }
    Ok(())
}

#[async_trait]
impl PaymentsPort for StubPayments {
    async fn charge(&self, req: ChargeRequest) -> Result<ChargeReceipt, PaymentError> {
        validate(&req)?;
        // Idempotency replay: same key already charged -> identical receipt.
        if let Ok(charges) = self.state.charges.lock() {
            if let Some(existing) = charges.get(&req.idempotency_key) {
                return Ok(existing.clone());
            }
        }
        if let Some(err) = self.take_scripted(&req.idempotency_key) {
            return Err(err);
        }
        let receipt = ChargeReceipt {
            receipt_id: self.next_receipt_id(),
            status: ChargeStatus::Charged,
        };
        if let Ok(mut charges) = self.state.charges.lock() {
            charges.insert(req.idempotency_key, receipt.clone());
        }
        Ok(receipt)
    }

    async fn refund(
        &self,
        receipt_id: &str,
        amount_minor: u64,
    ) -> Result<RefundReceipt, PaymentError> {
        if amount_minor == 0 {
            return Err(PaymentError::Invalid {
                amount_minor,
                currency: CurrencyCode::Usd,
            });
        }
        let known = match self.state.charges.lock() {
            Ok(c) => c.values().any(|r| r.receipt_id == receipt_id),
            Err(_) => false,
        };
        if !known {
            return Err(PaymentError::Refused(format!(
                "unknown receipt {receipt_id}"
            )));
        }
        let n = self.state.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let refund = RefundReceipt {
            refund_id: format!("stub_refund_{n}"),
            amount_minor,
        };
        if let Ok(mut refunds) = self.state.refunds.lock() {
            refunds
                .entry(receipt_id.to_string())
                .or_default()
                .push(amount_minor);
        }
        Ok(refund)
    }
}
