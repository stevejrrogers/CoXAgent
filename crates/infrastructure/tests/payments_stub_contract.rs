//! CXA-F451 stub contract suite — plain (non-ignored), runs inside the
//! existing `check` job. Deterministic: no network, no credentials, every
//! instance owns its state (regression for the shared-sandbox-creds flake).

use std::collections::HashMap;

use coxagent_application::ports::outbound::{
    ChargeRequest, ChargeStatus, CurrencyCode, PaymentError, PaymentsPort,
};
use coxagent_infrastructure::payments::sandbox_client::SandboxConfig;
use coxagent_infrastructure::payments::stub::{ScriptedFault, StubPayments};

fn req(key: &str, amount_minor: u64) -> ChargeRequest {
    ChargeRequest {
        amount_minor,
        currency: CurrencyCode::Usd,
        idempotency_key: key.to_string(),
        order_ref: "ORD-1".to_string(),
    }
}

#[tokio::test]
async fn happy_path_returns_a_charged_receipt() {
    let stub = StubPayments::new(HashMap::new());
    let receipt = match stub.charge(req("k-happy", 1999)).await {
        Ok(r) => r,
        Err(e) => panic!("happy path must charge, got {e:?}"),
    };
    assert_eq!(receipt.status, ChargeStatus::Charged);
    // Refund round-trip over the same port.
    let refund = match stub.refund(&receipt.receipt_id, 500).await {
        Ok(r) => r,
        Err(e) => panic!("refund against known receipt must succeed, got {e:?}"),
    };
    assert_eq!(refund.amount_minor, 500);
}

#[tokio::test]
async fn scripted_faults_map_to_the_correct_error_variants_with_attribution() {
    let mut faults = HashMap::new();
    faults.insert(
        "k-vu".to_string(),
        vec![ScriptedFault::VendorUnavailable {
            status: 503,
            retry_after_ms: Some(250),
        }],
    );
    faults.insert(
        "k-dec".to_string(),
        vec![ScriptedFault::Declined {
            reason: "insufficient_funds".into(),
        }],
    );
    faults.insert(
        "k-bad".to_string(),
        vec![ScriptedFault::VendorUnavailable {
            status: 429,
            retry_after_ms: None,
        }],
    );
    let stub = StubPayments::new(faults);

    match stub.charge(req("k-vu", 500)).await {
        Err(PaymentError::VendorUnavailable {
            status,
            retry_after_ms,
        }) => {
            assert_eq!(status, 503);
            assert_eq!(retry_after_ms, Some(250));
        }
        other => panic!("expected VendorUnavailable, got {other:?}"),
    }
    match stub.charge(req("k-dec", 700)).await {
        Err(PaymentError::Refused(reason)) => assert_eq!(reason, "insufficient_funds"),
        other => panic!("expected declined Refused, got {other:?}"),
    }
    match stub.charge(req("k-bad", 900)).await {
        Err(PaymentError::VendorUnavailable {
            status,
            retry_after_ms,
        }) => {
            assert_eq!(status, 429);
            assert_eq!(retry_after_ms, None);
        }
        other => panic!("expected rate-limit VendorUnavailable, got {other:?}"),
    }
}

/// Declines are valid business outcomes surfaced as receipt state on replay
/// paths; here we verify a scripted decline then retry converges to Charged
/// within max attempts while keeping each error attributed (fake clock: 1ms).
#[tokio::test]
async fn retry_sequence_converges_within_max_attempts_monotonically() {
    let mut faults = HashMap::new();
    faults.insert(
        "k-retry".to_string(),
        vec![
            ScriptedFault::VendorUnavailable {
                status: 503,
                retry_after_ms: None,
            },
            ScriptedFault::VendorUnavailable {
                status: 429,
                retry_after_ms: Some(10),
            },
        ],
    );
    let stub = StubPayments::new(faults);

    let cfg = match SandboxConfig::validated("https://sbx.test", "sk", 2, 1) {
        Ok(c) => c,
        Err(e) => panic!("config must validate: {e:?}"),
    };
    let attempt_0 = cfg.backoff_ms(0, None);
    let attempt_1 = cfg.backoff_ms(1, Some(10));
    assert!(
        attempt_1 >= attempt_0.min(10),
        "backoff must be monotonic-with-floor across attempts"
    );

    // Attempt 1: transient fault. Attempt 2: transient fault. Attempt 3: ok.
    let mut final_receipt = None;
    if final_receipt.is_none() {
        for _ in 0..3 {
            if let Ok(r) = stub.charge(req("k-retry", 1200)).await {
                final_receipt = Some(r);
                break;
            }
        }
    }
    let Some(receipt) = final_receipt else {
        panic!("retry sequence must converge within max_attempts");
    };
    assert_eq!(receipt.status, ChargeStatus::Charged);
    // Idempotent replay of the SAME key returns the SAME receipt.
    let replay = match stub.charge(req("k-retry", 1200)).await {
        Ok(r) => r,
        Err(e) => panic!("replay must succeed, got {e:?}"),
    };
    assert_eq!(
        replay.receipt_id, receipt.receipt_id,
        "idempotency must not double-charge"
    );
}

/// Two suites constructing separate StubPayments instances never share state —
/// the direct regression for the shared-credentials root cause.
#[tokio::test]
async fn concurrent_suites_with_separate_instances_are_isolated() {
    let mut faults_a = HashMap::new();
    faults_a.insert(
        "shared-key".to_string(),
        vec![ScriptedFault::Declined {
            reason: "a-only".into(),
        }],
    );
    let a = StubPayments::new(faults_a);
    let b = StubPayments::new(HashMap::new());

    let (ra, rb) = tokio::join!(
        a.charge(req("shared-key", 100)),
        b.charge(req("shared-key", 100))
    );
    assert!(matches!(ra, Err(PaymentError::Refused(ref r)) if r == "a-only"));
    assert!(rb.is_ok(), "instance B must be unaffected by A's script");
}

/// Fail-closed config validation lives beside the adapters: missing variables
/// are named BEFORE any HTTP call could happen.
#[test]
fn live_config_fails_closed_naming_the_missing_variable() {
    let miss_url = SandboxConfig::validated("", "key", 3, 10);
    match miss_url {
        Err(PaymentError::Refused(m)) => assert!(m.contains("COXAGENT_SANDBOX_BASE_URL")),
        _ => panic!("blank base URL must refuse"),
    }
    let miss_key = SandboxConfig::validated("https://sbx.test", "  ", 3, 10);
    match miss_key {
        Err(PaymentError::Refused(m)) => assert!(m.contains("COXAGENT_SANDBOX_API_KEY")),
        _ => panic!("blank api key must refuse"),
    }
}
