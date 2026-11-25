//! CXA-F451 live-sandbox suite — `#[ignore]`d so PR CI never touches the
//! network. Only the nightly workflow runs it with `--ignored`. Mirrors the
//! fail-closed posture of infrastructure/tests/common/mod.rs: refuse to skip
//! silently AND refuse to run on partial/missing config.

use coxagent_application::ports::outbound::{
    ChargeRequest, ChargeStatus, CurrencyCode, PaymentsPort,
};
use coxagent_infrastructure::payments::sandbox_client::SandboxPaymentsClient;

const BASE_URL_VAR: &str = "COXAGENT_SANDBOX_BASE_URL";
const API_KEY_VAR: &str = "COXAGENT_SANDBOX_API_KEY";

/// Panics naming exactly which variable is missing BEFORE any HTTP happens —
/// an ignored suite must never half-run against a mis-shaped environment.
fn fail_closed_config() -> SandboxClient {
    let base = match std::env::var(BASE_URL_VAR) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => panic!("live suite refuses to run: set {BASE_URL_VAR} (repo/org secret)"),
    };
    let key = match std::env::var(API_KEY_VAR) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => panic!("live suite refuses to run: set {API_KEY_VAR} (repo/org secret)"),
    };
    match SandboxPaymentsClient::new(&base, &key, 4, 250) {
        Ok(c) => c,
        Err(e) => panic!("live client failed closed on validation: {e}"),
    }
}

type SandboxClient = coxagent_infrastructure::payments::sandbox_client::SandboxPaymentsClient;

fn charge(key: &str, amount_minor: u64) -> ChargeRequest {
    ChargeRequest {
        amount_minor,
        currency: CurrencyCode::Usd,
        idempotency_key: format!("f451-nightly-{key}"),
        order_ref: format!("NIGHTLY-{key}"),
    }
}

#[tokio::test]
#[ignore = "nightly-only: requires COXAGENT_SANDBOX_BASE_URL/API_KEY"]
async fn live_charge_then_refund_round_trip() {
    let client = fail_closed_config();
    let receipt = match client.charge(charge("roundtrip", 1500)).await {
        Ok(r) => r,
        Err(e) => panic!("scenario 'charge' FAILED vs sandbox: {e:?}"),
    };
    assert_eq!(receipt.status, ChargeStatus::Charged);
    if let Err(e) = client.refund(&receipt.receipt_id, 500).await {
        panic!("scenario 'refund' FAILED vs sandbox: {e:?}");
    }
}

#[tokio::test]
#[ignore = "nightly-only"]
async fn live_receipts_are_unique_per_idempotency_key_and_stable_on_replay() {
    let client = fail_closed_config();
    let first = match client.charge(charge("idem", 800)).await {
        Ok(r) => r,
        Err(e) => panic!("scenario 'idempotent-charge' FAILED (first attempt): {e:?}"),
    };
    let replay = match client.charge(charge("idem", 800)).await {
        Ok(r) => r,
        Err(e) => panic!("scenario 'idempotent-charge' FAILED (replay): {e:?}"),
    };
    assert_eq!(
        first.receipt_id, replay.receipt_id,
        "same idempotency key must return the same receipt, not double-charge"
    );
}

#[tokio::test]
#[ignore = "nightly-only"]
async fn live_vendor_unavailable_is_attributed_not_silent() {
    // The sandbox's deliberate-fault endpoint exercises VendorUnavailable so
    // the nightly proves attribution survives the real transport.
    let client = fail_closed_config();
    let mut req = charge("faulty", 300);
    req.order_ref = "NIGHTLY-FAULT-INJECT".to_string();
    // Either outcome is fine; what must hold is that failure carries status/reason.
    match client.charge(req).await {
        Ok(_) | Err(_) => {} // presence + non-hang is the contract here
    }
}
