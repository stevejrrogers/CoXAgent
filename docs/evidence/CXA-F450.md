# CXA-F450 — Mock payment-retry harness (evidence)

Deterministic, offline stand-in for the flaky card-provider sandbox so QA can
exercise every locked retry path without touching shared sandbox credentials
(team lesson: parallelised tests hammering one sandbox erode pipeline trust).

## What shipped

- `crates/application/src/mock_payment_harness.rs` — PURE core (zero IO;
  passes `hexagonal_gate.rs`). Timestamps are injected by the caller, attempts
  append to an ordered, mutex-guarded log.
- `crates/app/tests/payment_mock_harness.rs` — end-to-end suite driving the
  harness through all four acceptance paths.
- Pre-existing guard `crates/app/tests/payment_mock_harness_cxa_f450_tdd.rs`
  now passes against the real module/suite it required.

## Public contract (`coxagent_application::mock_payment_harness`)

| Item | Meaning |
|---|---|
| `MockPaymentHarness::retryable_decline(retry_window, max_retries)` | AC1 scenario constructor |
| `MockPaymentHarness::permanent_failure(code, max_retries)` | AC2 scenario constructor |
| `authorize(consumer_id, timestamp) -> HarnessResponse` | send one mocked attempt |
| `attempts() -> Result<Vec<AttemptRecord>, String>` | ordered attempt log (AC4 attribution) |
| `PaymentStatus::{Approved, Declined, RetriesExhausted}` | terminal outcomes incl. explicit exhaustion signal (AC3) |
| `RETRYABLE_CODE = "retryable_decline"`, `"window_closed"` | stable failure codes; distinct 'retry me' vs 'too late' |

## Scenario → command → expected trail

Run from repo root:

```
cargo test -p coxagent-application --lib mock_payment_harness   # 5 unit tests
cargo test -p coxagent-app --test payment_mock_harness          # 4 e2e tests
cargo test -p coxagent-app --test payment_mock_harness_cxa_f450_tdd
```

### AC1 — RetryableDecline (decline first, approve inside window)
Call `authorize("checkout-web", t0)` → `Declined` / `retryable_decline`;
`authorize(.., tN≤retry_window)` → `Approved`. Past the window → `Declined` /
`window_closed` (distinct code). Log records each leg with consumer id,
injected timestamp, outcome and rule name.

### AC2 — PermanentFailure (one stable code, never approves)
Every `authorize` returns `Declined` with the SAME configured code (e.g.
`issuer_unavailable`); rule attributed as `permanent_failure`.

### AC3 — configurable max-retries limit N
Attempt index ≤ N answers normally; index > N (and everything after) yields
the explicit `RetriesExhausted` status until a new session is constructed.

### AC4 — concurrent consumers, single ordered log
Four threads × 25 `authorize` calls interleave into one strictly
monotonic-by-attempt-index log of 100 entries, 25 per consumer, each stamped
with its rule — verified under `std::thread` concurrency.

## Determinism / hermeticity

No ports bound, no network, no RNG, no sleep: timestamps are arguments, order
is log-position, codes are constants. Fully CI-safe and credential-free.
