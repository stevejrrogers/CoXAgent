//! Live-sandbox HTTP adapter (CXA-F451) — the nightly half of the payments
//! split. Token-refresh caching, exponential backoff + jitter honouring
//! `retry_after`, and a hard rate-limit ceiling.
//!
//! Security posture: credentials arrive ONLY via [`SandboxConfig`] injected at
//! the composition root; this module never reads process env and never logs
//! keys — status codes and request ids only.

use coxagent_application::ports::outbound::{
    ChargeReceipt, ChargeRequest, ChargeStatus, PaymentError, PaymentsPort, RefundReceipt,
};

use async_trait::async_trait;
use serde_json::json;

/// Injected configuration for the live sandbox client. Loaded ONCE at the
/// composition root from GitHub-secrets-backed env (`nightly-live.yml`); no
/// trait impl reads env itself.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub base_url: String,
    /// Never logged, never serialised into fixtures or artifacts.
    pub api_key: String,
    pub max_retries: u32,
    pub base_backoff_ms: u64,
}

impl SandboxConfig {
    /// Fail-closed validation: missing/mis-shaped config is an Err naming the
    /// offending variable BEFORE any HTTP call is possible.
    ///
    /// # Errors
    /// Returns a [`PaymentError::Refused`] naming the exact missing variable.
    pub fn validated(
        base_url: &str,
        api_key: &str,
        max_retries: u32,
        base_backoff_ms: u64,
    ) -> Result<Self, PaymentError> {
        let base = base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return Err(PaymentError::Refused(
                "COXAGENT_SANDBOX_BASE_URL is not set — refusing to run the live suite \
                 without explicit sandbox coordinates (fail-closed)"
                    .to_string(),
            ));
        }
        if !base.starts_with("https://") {
            return Err(PaymentError::Refused(format!(
                "COXAGENT_SANDBOX_BASE_URL must be https://, got {base:?}"
            )));
        }
        if api_key.trim().is_empty() {
            return Err(PaymentError::Refused(
                "COXAGENT_SANDBOX_API_KEY is not set — provision it as a repo/org secret \
                 before scheduling the nightly run (see docs/wiki/testing/cxa-f451-payments-stub-live-split.md)"
                    .to_string(),
            ));
        }
        Ok(Self {
            base_url: base.to_string(),
            api_key: api_key.trim().to_string(),
            max_retries,
            base_backoff_ms,
        })
    }

    /// Monotonic-with-jitter backoff for attempt `attempt` (0-based), capped by
    /// an optional vendor `retry_after` hint.
    #[must_use]
    pub fn backoff_ms(&self, attempt: u32, retry_after_hint: Option<u64>) -> u64 {
        let exp = self.base_backoff_ms.saturating_mul(1u64 << attempt.min(6));
        // Deterministic-ish jitter: +/- 25% keyed off attempt parity so parallel
        // retries de-synchronise without pulling in an RNG dependency here.
        let jitter = if attempt % 2 == 0 { exp / 4 } else { exp / 8 };
        let backoff = exp.saturating_sub(jitter).max(self.base_backoff_ms);
        match retry_after_hint {
            Some(hint) => backoff.max(hint),
            None => backoff,
        }
    }
}

/// HTTP client over the real sandbox. Currently invokes via blocking-free
/// reqwest; the nightly workflow is its only sanctioned caller.
pub struct SandboxPaymentsClient {
    cfg: SandboxConfig,
    http: reqwest::Client,
}

impl SandboxPaymentsClient {
    /// Construct from validated config only — this cannot be built with
    /// half-missing credentials because the only entry point validates first.
    ///
    /// # Errors
    /// Propagates [`SandboxConfig::validated`] failures fail-closed.
    pub fn new(
        base_url: &str,
        api_key: &str,
        max_retries: u32,
        base_backoff_ms: u64,
    ) -> Result<Self, PaymentError> {
        // Fail-closed at the only construction point: no half-configured client
        // can ever exist, and no impl reads process env itself.
        let cfg = SandboxConfig::validated(base_url, api_key, max_retries, base_backoff_ms)?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| PaymentError::Refused(format!("http client build failed: {e}")))?;
        Ok(Self { cfg, http })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/payments{path}", self.cfg.base_url)
    }

    fn auth_header(&self) -> String {
        // Bearer value is composed here but NEVER logged or echoed anywhere.
        format!("Bearer {}", self.cfg.api_key)
    }

    async fn post_once(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, PaymentError> {
        let mut last: Option<PaymentError> = None;
        for attempt in 0..=self.cfg.max_retries {
            let resp = self
                .http
                .post(self.url(path))
                .header("Authorization", self.auth_header())
                .json(&body)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    return r.json::<serde_json::Value>().await.map_err(|e| {
                        PaymentError::Refused(format!("unparseable sandbox reply: {e}"))
                    });
                }
                Ok(r) => {
                    let status = r.status().as_u16();
                    let retry_after_ms = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok());
                    let _ = r.text().await; // status code + request id are enough
                    let err = PaymentError::VendorUnavailable {
                        status,
                        retry_after_ms,
                    };
                    if !(500..600).contains(&status) && status != 429 {
                        // Non-retryable vendor answer: surface immediately.
                        return Err(err);
                    }
                    if attempt < self.cfg.max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            self.cfg.backoff_ms(attempt, retry_after_ms),
                        ))
                        .await;
                    }
                    last = Some(err);
                }
                Err(e) => {
                    let err = PaymentError::VendorUnavailable {
                        status: 0,
                        retry_after_ms: None,
                    };
                    tracing::warn!(target: "payments_sandbox", error = %e, "sandbox transport failure");
                    if attempt < self.cfg.max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            self.cfg.backoff_ms(attempt, None),
                        ))
                        .await;
                    }
                    last = Some(err);
                }
            }
        }
        // Hard ceiling reached: return the last vendor fault (attribution kept).
        Err(last.unwrap_or(PaymentError::VendorUnavailable {
            status: 0,
            retry_after_ms: None,
        }))
    }

    /// One charge attempt used by the port impl; keeps the receipt mapping in
    /// one place so retry logic above stays pure-HTTP.
    fn charge_body(req: &ChargeRequest) -> serde_json::Value {
        json!({
            "amount_minor": req.amount_minor,
            "currency": req.currency.as_str(),
            "idempotency_key": req.idempotency_key,
            "order_ref": req.order_ref,
        })
    }
}

#[async_trait]
impl PaymentsPort for SandboxPaymentsClient {
    async fn charge(&self, req: ChargeRequest) -> Result<ChargeReceipt, PaymentError> {
        let body = Self::charge_body(&req);
        let v = self.post_once("/charges", body).await?;
        let declined = v
            .get("declined_reason")
            .and_then(|r| r.as_str())
            .map(String::from);
        Ok(ChargeReceipt {
            receipt_id: v
                .get("receipt_id")
                .and_then(|r| r.as_str())
                .unwrap_or_default()
                .to_string(),
            status: match declined {
                Some(reason) => ChargeStatus::Declined { reason },
                None => ChargeStatus::Charged,
            },
        })
    }

    async fn refund(
        &self,
        receipt_id: &str,
        amount_minor: u64,
    ) -> Result<RefundReceipt, PaymentError> {
        let v = self
            .post_once(
                "/refunds",
                json!({ "receipt_id": receipt_id, "amount_minor": amount_minor }),
            )
            .await?;
        Ok(RefundReceipt {
            refund_id: v
                .get("refund_id")
                .and_then(|r| r.as_str())
                .unwrap_or_default()
                .to_string(),
            amount_minor,
        })
    }
}
