//! TDD tests for CXA-F414 — payment adapter retry logic (P1 checkout cluster).
//! RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "Given a transient failure (e.g. 5xx or timeout) from the payment
//!    gateway during checkout, when the adapter retries and succeeds on a
//!    later attempt, then the customer's order completes without error and
//!    they see a normal confirmation screen with exactly one charge."
//! 2. "Given a payment request that may have reached the gateway before a
//!    retry (ambiguous outcome), when the adapter retries, then no duplicate
//!    charge is created for the same cart/checkout session — verified by
//!    end-to-end test producing at most one settled transaction."
//! 3. "Given permanent failures (card declined, invalid credentials/auth
//!    error) from the gateway, when the adapter classifies them as
//!    non-retryable, then checkout stops immediately, shows the user an
//!    actionable message within ~2s, and no further retry attempts are made."
//! 4. "Given the gateway remains unavailable through all retry attempts, when
//!    backoff ends, then the customer sees a clear 'payment could not be
//!    completed' state explaining what to do next (retry option / saved cart),
//!    their cart and entered details are preserved, and nothing is left in a
//!    partial paid/unpaid limbo."
//! 5. "Given a retried or failed payment flow, when support inspects it, then
//!    every attempt, its outcome and any delay appear in the order/payment
//!    audit trail with timestamps."
//!
//! HOW THESE CRITERIA ARE ENCODED — the bootstrap_lessons_f371_tdd.rs /
//! lesson_efficacy_f306_tdd.rs convention: pure fixtures over the real types
//! the codebase has today, plus source-scan red guards over the surfaces that
//! do not exist yet. No fake HTTP server, no host harness, no network port,
//! no invented identifiers; every failing assertion fails only because
//! CXA-F414's behaviour is missing.
//!
//! The real codebase data every fixture uses:
//!   * the retry/backoff + bounded-attempts semantics already live as PURE
//!     functions in `application::state::outbox`
//!     (`advance_after_failure`, `backoff_delay_secs`, `OUTBOX_MAX_ATTEMPTS`,
//!     `OutboxStatus::{Pending, Dead}`) — the proven in-house shape the
//!     payment adapter's retry loop must mirror;
//!   * the terminal-state shell contract a payment-failure panel renders
//!     under is `domain::panel_terminal_state::PanelEvent::{
//!     DataBecameEmpty, DataErrored}` — a panel may enter ERROR only
//!     with an attributed cause, which is AC4's "clear state";
//!   * the durable per-incident ledger shape is
//!     `application::state::ops::EngineIncident` and the audit sink port is
//!     `application::ports::outbound::audit::AuditPort` — the homes an
//!     attempt-with-timestamp record rides for AC5.
//!
//! DESIGN GAPS FLAGGED FOR THE SA (blocking the green half, encoded below as
//! red guards, never fabricated):
//!   * NO payment/checkout/order/cart aggregate, charge, refund or
//!     "settled transaction" type exists anywhere in the workspace
//!     (`charge|cart|order_id|refund|checkout session` match nothing outside
//!     doc comments). AC1/AC2 name customer-facing orders, charges and
//!     confirmation screens — there is no domain model to complete, no
//!     read model to render, and no place a settlement can land. ASK SA.
//!   * AC2 hinges on an idempotency key scoped to a cart/checkout session;
//!     neither the key nor the session id exists.
//!   * AC1/AC3/AC4 speak of a customer-visible checkout UI ("confirmation
//!     screen", "actionable message within ~2s"); the web app has no
//!     checkout surface at all (`grep -ri checkout web/src` is empty).
//!   * AC5 says "order/payment audit trail"; `ProjectState` has no
//!     payment-audit field and `AuditPort` carries security events keyed by
//!     actor/action, not payment attempts.
//!
//! AC → test map:
//! - AC1: [`ac1_transient_failure_retries_to_a_single_settled_charge`] (RED)
//! - AC2: [`ac2_ambiguous_retry_reuses_an_idempotency_key_per_checkout_session`] (RED)
//! - AC3: [`ac3_permanent_failures_are_non_retryable_and_stop_immediately`] (RED)
//! - AC4: [`ac4_exhausted_backoff_ends_in_a_clear_not_completed_state_with_cart_preserved`] (RED)
//! - AC5: [`ac5_every_attempt_outcome_and_delay_is_audited_with_timestamps`] (RED)
//! - green premise guards: [`the_outbox_retry_semantics_are_the_house_pattern_to_mirror`],
//!   [`a_panel_enters_error_only_with_an_attributed_cause`]

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use coxagent_application::state::{
    advance_after_failure, backoff_delay_secs, OutboxEntry, OutboxStatus, OUTBOX_BASE_BACKOFF_SECS,
    OUTBOX_MAX_ATTEMPTS,
};
use coxagent_domain::panel_terminal_state::{transition, PanelErrorCause, PanelEvent, Phase};
use time::OffsetDateTime;

// --- accepted homes for the missing payment behaviour ------------------------

/// Where the gateway PORT belongs (hexagonal: application defines it, infra
/// implements it, IO never leaks into domain/application logic).
const PAYMENT_PORT_HOMES: &[&str] = &[
    "crates/application/src/ports/outbound/payment_gateway.rs",
    "crates/application/src/ports/outbound/payments.rs",
];

/// Where the retrying adapter belongs (infrastructure implements the port).
const PAYMENT_ADAPTER_HOMES: &[&str] = &[
    "crates/infrastructure/src/payment_gateway_http.rs",
    "crates/infrastructure/src/payments.rs",
    "crates/infrastructure/src/stripe.rs",
];

/// Where the order/checkout domain model would belong.
const ORDER_DOMAIN_HOMES: &[&str] = &[
    "crates/domain/src/order.rs",
    "crates/domain/src/checkout.rs",
    "crates/domain/src/payment.rs",
];

/// Where the customer-facing checkout routes/views live.
const CHECKOUT_UI_HOMES: &[&str] = &["web/src", "crates/presentation/src"];

// --- repo-state scan helpers (the f371/f306 pattern) --------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn try_read(rel: &str) -> Option<String> {
    let p = repo_root().join(rel);
    if p.is_dir() {
        return Some(read_tree(&p));
    }
    std::fs::read_to_string(p).ok()
}

fn read_tree(dir: &Path) -> String {
    let mut buf = String::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
            if name == "node_modules" {
                    continue;
                }
                buf.push_str(&read_tree(&p));
            } else {
                let ext = p.extension().unwrap_or_default().to_string_lossy();
                if ext == "rs" || ext == "js" || ext == "ts" {
                    buf.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
                }
            }
        }
    }
    buf
}

fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase()
}

fn any_of(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

// --- green premise guards: the pattern to mirror, over real types -------------

/// The house retry semantics the payment adapter must not reinvent worse:
/// bounded attempts, exponential backoff, a dead terminal state — all PURE.
#[test]
fn the_outbox_retry_semantics_are_the_house_pattern_to_mirror() {
    assert_eq!(backoff_delay_secs(1), OUTBOX_BASE_BACKOFF_SECS);
    assert!(backoff_delay_secs(8) > backoff_delay_secs(1), "backoff grows");
    let base = OutboxEntry {
        id: 7,
        kind: "payment_intent".to_owned(),
        project: "demo".to_owned(),
        message: "charge attempt".to_owned(),
        status: OutboxStatus::Pending,
        attempts: 0,
        next_attempt_at: 0,
        created_at: 0,
    };
    // A transient failure schedules another attempt…
    let after_one = advance_after_failure(1_000, &base);
    assert_eq!(after_one.status, OutboxStatus::Pending);
    assert!(after_one.next_attempt_at > 1_000);
    // …and exhausting the budget parks the entry DEAD, never limbo.
    let mut e = base.clone();
    for _ in 0..OUTBOX_MAX_ATTEMPTS {
        e = advance_after_failure(e.next_attempt_at, &e);
    }
    assert_eq!(e.status, OutboxStatus::Dead, "exhaustion terminates, no limbo");
}

/// AC4's shell premise: a failure panel enters ERROR only WITH an attributed
/// cause — the exact contract a 'payment could not be completed' state must
/// satisfy (the WHY is mandatory here, so it cannot silently idle).
#[test]
fn a_panel_enters_error_only_with_an_attributed_cause() {
    let now = OffsetDateTime::now_utc();
    // The ONLY edge into Error carries a validated, non-blank cause — the WHY
    // a 'payment could not be completed' panel must show (AC4's premise).
    let err = transition(
        Phase::<()>::Skeleton,
        PanelEvent::FetchFailed {
            cause: PanelErrorCause::new("gateway unreachable after all retries")
                .expect("validated cause"),
            retryable: true,
        },
        now,
    );
    match err {
        Phase::Error { cause, .. } => {
            assert_eq!(cause.as_str(), "gateway unreachable after all retries");
        }
        other => panic!("expected Error phase with attributed cause, got {other:?}"),
    }
}

// --- AC1 ----------------------------------------------------------------------

/// AC1: a transient 5xx/timeout is retried; a later success settles EXACTLY
/// one charge and checkout completes. RED: no payment gateway port, no
/// adapter, and no order/charge state exists anywhere to settle into.
#[test]
fn ac1_transient_failure_retries_to_a_single_settled_charge() {
    let port_exists = PAYMENT_PORT_HOMES.iter().any(|h| {
        try_read(h).is_some_and(|src| {
            let f = flat(&src);
            any_of(&f, &["traitpaymentgatewayport", "trait payment_gateway_port"])
                && f.contains("charge")
        })
    });
    assert!(
        port_exists,
        "no PaymentGatewayPort exists in {PAYMENT_PORT_HOMES:?} — nothing \
         accepts a charge, so no retry-and-settle behaviour can exist"
    );
    let adapter_retries = PAYMENT_ADAPTER_HOMES.iter().any(|h| {
        try_read(h).is_some_and(|src| {
            let f = flat(&src);
            f.contains("transient") && (f.contains("retry") || f.contains("attempt"))
        })
    });
    assert!(
        adapter_retries,
        "no infrastructure adapter classifies transient vs retryable outcomes — \
         a 5xx today aborts checkout instead of completing on a later attempt"
    );
    let order_model = ORDER_DOMAIN_HOMES.iter().any(|h| try_read(h).is_some());
    assert!(
        order_model,
        "no order/checkout/payment domain model exists — there is no order to \
         complete and no charge count to pin 'exactly one' against (DESIGN GAP)"
    );
}

// --- AC2 ----------------------------------------------------------------------

/// AC2: an ambiguous outcome (request MAY have reached the gateway) retried
/// under the SAME checkout session must yield at most one settled charge.
/// The mechanism is an idempotency key scoped to the session. RED: neither
/// idempotency keys nor checkout-session ids exist anywhere.
#[test]
fn ac2_ambiguous_retry_reuses_an_idempotency_key_per_checkout_session() {
    let found = [
        PAYMENT_PORT_HOMES,
        PAYMENT_ADAPTER_HOMES,
        ORDER_DOMAIN_HOMES,
    ]
    .concat()
    .iter()
    .any(|h| {
        try_read(h).is_some_and(|src| {
            let f = flat(&src);
            f.contains("idempoten")
                && (f.contains("checkoutsess") || f.contains("cart"))
        })
    });
    assert!(
        found,
        "no idempotency-key-per-checkout-session logic exists — a retried \
         ambiguous request would create a second settled charge (DESIGN GAP: \
         the checkout-session id itself does not exist in the codebase)"
    );
}

// --- AC3 ----------------------------------------------------------------------

/// AC3: card declines and invalid-credential errors are classified
/// NON-retryable: zero further attempts, an actionable message, fast (<~2s).
/// RED: no classifier and no decline surface exist.
#[test]
fn ac3_permanent_failures_are_non_retryable_and_stop_immediately() {
    let classifier = PAYMENT_ADAPTER_HOMES
        .iter()
        .chain(PAYMENT_PORT_HOMES)
        .any(|h| {
            try_read(h).is_some_and(|src| {
                let f = flat(&src);
                any_of(&f, &["nonretryable", "non_retryable", "permanentfailure"])
                    && f.contains("declin")
            })
        });
    assert!(
        classifier,
        "nothing classifies card-declined/auth errors as non-retryable — \
         checkout would burn the full retry budget on a guaranteed-loss path"
    );
    let ui = CHECKOUT_UI_HOMES.iter().find_map(|h| try_read(h));
    let actionable = ui
        .as_deref()
        .map(flat)
        .is_some_and(|f| f.contains("decline") || f.contains("insufficient"));
    assert!(
        actionable,
        "no checkout UI surface renders an actionable decline message (DESIGN \
         GAP: no checkout view exists at all)"
    );
}

// --- AC4 ----------------------------------------------------------------------

/// AC4: when ALL attempts fail, the customer lands in a clear
/// 'payment could not be completed' state with a next action, cart and
/// entered details PRESERVED, and no partial-paid limbo. RED: no checkout
/// state machine, no cart-preservation surface exists.
#[test]
fn ac4_exhausted_backoff_ends_in_a_clear_not_completed_state_with_cart_preserved() {
    let exhaustive_end = [
        PAYMENT_ADAPTER_HOMES,
        CHECKOUT_UI_HOMES,
    ]
    .concat()
    .iter()
    .any(|h| {
        try_read(h).is_some_and(|src| {
            let f = flat(&src);
            f.contains("couldnotbecompleted") || f.contains("paymentfailed")
        })
    });
    assert!(
        exhaustive_end,
        "no 'payment could not be completed' terminal wording exists — the \
         exhausted-backoff outcome is unspecified/unrenderable today"
    );
    let preserved = CHECKOUT_UI_HOMES
        .iter()
        .any(|h| try_read(h).is_some_and(|src| flat(&src).contains("savedcart")));
    assert!(
        preserved,
        "no save-the-cart-across-a-payment-failure state exists (DESIGN GAP: \
         the cart itself does not exist in the codebase)"
    );
}

// --- AC5 ----------------------------------------------------------------------

/// AC5: every attempt — outcome AND delay — lands in the order/payment audit
/// trail with timestamps, inspectable by support. RED: `AuditPort` records
/// security events keyed by actor/action; no payment-attempt record, no
/// per-order trail and no `ProjectState` home for it exist.
#[test]
fn ac5_every_attempt_outcome_and_delay_is_audited_with_timestamps() {
    let audited = ["crates/application/src/ports/outbound/audit.rs", "crates/application/src/state/mod.rs"]
        .iter()
        .any(|h| {
            try_read(h).is_some_and(|src| {
                let f = flat(&src);
                (f.contains("paymentattempt") || f.contains("payment_attempt"))
                    && f.contains("delay")
            })
        });
    assert!(
        audited,
        "no payment-attempt audit record carrying outcome + delay + timestamp \
         exists — support cannot reconstruct a retried/failed checkout (see \
         AuditPort: security events only)"
    );
}
