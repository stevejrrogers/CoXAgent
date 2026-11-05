FOLDER: Engineering

# Payment Adapter Isolation (Stub + Contract Tests)

**Keywords:** payment port, StubPaymentPort, HttpPaymentPort, PaymentError, contract tests, CI isolation, idempotency, webhook verification, COXAGENT_PAYMENT_ENDPOINT, fault scenarios

## Overview

CXA-F418 isolates every live-payment code path from CI and local development behind one outbound port (`PaymentPort`) with two interchangeable adapters: a fully deterministic in-process stub that runs by default, and an HTTP adapter for the real sandbox provider that is opt-in via environment variable. It exists so the pipeline stays green regardless of whether the payment provider's credentials are present or revoked, while still exercising real failure modes (timeout, declined charge, connection drop) through scripted scenarios rather than network flake.

## How it works

The architecture follows the standard hexagonal pattern: the application layer defines the port, infrastructure implements it twice, and the composition root selects between them at startup.

- **The port.** `trait PaymentPort` in crates/application/src/ports/outbound/payment.rs declares four operations: `authorize(ChargeRequest) -> Result<Authorization, PaymentError>`, `capture(auth_id, amount_minor) -> Receipt`, `refund(receipt_id, Option<amount_minor>) -> Refund`, and `verify_webhook(payload, WebhookSignature) -> WebhookEvent`. Authorization is idempotent per order: re-authorizing returns the ORIGINAL authorization flagged `duplicate = true`, never a second hold.
- **Errors are classified once.** `enum PaymentError` (`ProviderUnavailable` / `Declined` / `Duplicate` / `InvalidRequest`) is the shared vocabulary both adapters must map onto identically; drift between them is exactly what the contract suite fails red on. `code()` yields stable wire/log codes, and `is_provider_fault()` feeds the attribution rule in faults.rs — only `ProviderUnavailable` counts as a provider/environment fault and never charges the ticket.
- **Stub adapter.** `StubPaymentPort::new(webhook_secret)` keeps an internal ledger keyed by idempotency key plus an outcomes map filled via `script_outcome(order_id, StubOutcome)` where `StubOutcome` is `Success | Decline(String) | Timeout | NetworkFault(String)`. Unscripted orders take the plain success path. There is zero randomness, zero sleeping — same inputs always yield byte-identical results, which is what stops pipeline flake. Debug helpers `adapter_name()` ("StubPaymentPort (simulated)") and `authorize_raw_for_tests(order_id)` exist for provenance logging and deterministic replay assertions.
- **HTTP adapter.** `HttpPaymentPort::new(&HttpPaymentConfig)` speaks reqwest to the real sandbox. `PAYMENT_TIMEOUT` (10 s) bounds every call — a slow provider IS an unavailable provider. Transport errors become `ProviderUnavailable`; non-2xx status maps to `declined` / `invalid_request` / `provider_unavailable` by payload reason in `transport_error`/`provider_error`.
- **Selection, fail-closed.** In crates/app/src/builders.rs, `build_payment() -> Result<Arc<dyn PaymentPort>, String>` calls `HttpPaymentConfig::from_env()`: if `COXAGENT_PAYMENT_ENDPOINT` is unset/empty it logs `"payment adapter: StubPaymentPort"` and returns the stub; if set, the endpoint must parse as an absolute http(s) URL (`parse()`, pure unit-tested seam) or build_payment returns Err AT STARTUP. A present-but-broken config never silently downgrades to simulated payments. Selected-adapter provenance is logged either way (AC2).
- **Contract enforcement.** One shared conformance module, crates/infrastructure/tests/common/payment_conformance_shared.rs, is exercised against the port trait alone (same shape as `sql_store_contract`). Run 1 — stub conformance blocks CI unconditionally. Run 2 — the HTTP adapter run only executes when `COXAGENT_PAYMENT_ENDPOINT` is set (explicit skip, mirroring DSN-gated compose fixtures), never a silent red. Any future `PaymentPort` implementation must pass this exact suite unchanged.

CI wiring: `.github/workflows/ci.yml` job `payment-stub` (display name "payment service isolated behind stub (F418)") runs `cargo test -p coxagent-infrastructure --test payment_contract_f418 --test payment_stub_isolation_f418` with `COXAGENT_PAYMENT_ADAPTER=stub` and `COXAGENT_PAYMENT_FAULT_SCENARIOS="timeout,declined,connection-drop"`.

## Usage

Run the full local replay of the CI gate byte-identically (no GitHub needed):

```bash
cargo test -p coxagent-infrastructure --test payment_contract_f418 \
    --test payment_stub_isolation_f418
```

Script a failure mode deterministically:

```rust
use coxagent_infrastructure::payment_stub::{StubOutcome, StubPaymentPort};

let stub = StubPaymentPort::new("whsec_dev_default");
stub.script_outcome("ord-flaky", StubOutcome::Timeout);
// authorize(ChargeRequest::new("ord-flaky", ...)) now ALWAYS returns
// PaymentError::ProviderUnavailable("provider did not answer within the
// bounded window (order ord-flaky)")
```

Configure the real sandbox provider:

```bash
export COXAGENT_PAYMENT_ENDPOINT=https://api.sandbox.example.com
export COXAGENT_PAYMENT_API_KEY=sk-sandbox-...   # read at request time, never persisted
cargo run                                        # bad endpoint value aborts startup here
```

With nothing exported, you get simulated payments and a startup log line naming the selected adapter.

## Interface

`PaymentPort` (async trait, `Send + Sync`):

| Method | Signature | Notes |
|---|---|---|
| `authorize` | `(ChargeRequest) -> Result<Authorization, PaymentError>` | idempotent per order id/idempotency key; replays flag `duplicate: true` |
| `capture` | `(auth_id: &str, amount_minor: u64) -> Result<Receipt, PaymentError>` | funds against prior auth |
| `refund` | `(receipt_id: &str, amount_minor: Option<u64>) -> Result<Refund, PaymentError>` | `None` refunds whole remainder, `Some(n)` partial |
| `verify_webhook` | `(payload: &[u8], sig: &WebhookSignature) -> Result<WebhookEvent, PaymentError>` | HMAC-SHA256 hex digest over raw body checked BEFORE trusting payload; tampered/unknown-key → error |

Key types (crates/application/src/ports/outbound/payment.rs): `ChargeRequest{order_id, amount_minor, currency}` (+`idempotency_key()`), `Authorization{auth_id, order_id, amount_minor, duplicate}`, `Receipt{id, captured_minor}`, `Refund{id, amount_minor}`, `WebhookSignature{key_id, digest_hex}` (`::new(key_id, digest_hex)` convenience), `WebhookEvent{event_type, order_id, amount_minor}` (`charge.succeeded`, `charge.refunded`).

`PaymentError::code()` → `provider_unavailable` | `declined` | `duplicate` | `invalid_request`.

Test-file surface: `payment_contract_f418.rs` (suite runner), `payment_port_contract.rs` (pre-existing contract entry point), `payment_stub_isolation_f418.rs` (AC1/AC2/AC4 guard), `common/payment_conformance_shared.rs` (`webhook_digest_hex(secret, payload)`, `WEBHOOK_SECRET` fixture, per-order JSON bodies `{type, data:{order_id, amount_minor}}`).

## Configuration

| Variable | Default | Effect |
|---|---|---|
| `COXAGENT_PAYMENT_ENDPOINT` | unset → stub adapter | Absolute http(s) URL enables `HttpPaymentPort`; present but malformed FAILS STARTUP (`build_payment()` Err), never a silent downgrade to simulation |
| `COXAGENT_PAYMENT_API_KEY` | none | Bearer credential for the HTTP adapter, read per request from env; never committed or written to config files |
| `COXAGENT_PAYMENT_ADAPTER` | — | Provenance label set by CI (`"stub"`); startup also logs the chosen adapter regardless |
| `COXAGENT_PAYMENT_FAULT_SCENARIOS` | none | CI comma-list of scenarios to drive (`timeout,declined,connection-drop`); scenario triggering itself goes through `StubPaymentPort::script_outcome` |
| `PAYMENT_TIMEOUT` const | 10 s | Bounds every HTTP adapter call; a timeout classifies as `ProviderUnavailable` |

There is no UI or CLI surface; the feature ships as a seam plus CI gating ahead of the first real payment consumer.

## Edge cases and limits

- **No consumers yet.** `build_payment()` is currently `#[allow(dead_code)]` in builders.rs — wired into the hub builder as payment consumers land (the seam and CI isolation ship first). No endpoint in presentation charges anything today.
- **Idempotent replays legitimately return `duplicate: true`.** The fault-scenario isolation test deliberately asserts classification on the FIRST call only; duplicate-replay consistency lives in the shared conformance suite.
- **Poisoned-mutex panic is intentional** in the stub (`lock().expect(...)` — unrecoverable for an in-process harness), and clippy pedantic allows for `expect_used`/`must_use_candidate` are scoped to that file only; the workspace-wide `expect_used` hardening gates elsewhere still apply to new code.
- **Webhooks without a valid signature are rejected, never trusted** — both adapters verify identically; the contract suite proves accept/tamper symmetry using `hmac_digest_hex`.
- **`HttpPaymentPort` never runs in CI**, so transport behaviour of the real provider is only validated under explicit env opt-in (or in a deployment), not in the default pipeline.
- **Partial captures do NOT go through `capture`** — full-amount capture there; partial flows use refund semantics per provider terms.

## Code map

- crates/application/src/ports/outbound/payment.rs — the port: `PaymentPort` trait, all request/response/error types (`ChargeRequest`, `Authorization`, `WebhookSignature`, `WebhookEvent`, `PaymentError` with `code()`/`is_provider_fault()`).
- crates/application/src/ports/outbound/mod.rs — `pub mod payment;` + re-export of the port types.
- crates/infrastructure/src/payment_stub.rs — `StubPaymentPort`, `StubOutcome`, scripted-outcome ledger, deterministic HMAC helper, debug/replay helpers. ~305 lines.
- crates/infrastructure/src/payment_http.rs — `HttpPaymentPort`, `HttpPaymentConfig::from_env()/parse()`, `transport_error`/`provider_error` mapping, `PAYMENT_TIMEOUT`. ~225 lines.
- crates/app/src/builders.rs — `build_payment()` adapter selection with fail-closed validation and startup provenance logging (~line 1328).
- crates/infrastructure/tests/common/payment_conformance_shared.rs — ONE shared conformance module over the trait; `webhook_digest_hex`, `WEBHOOK_SECRET`, body fixtures.
- crates/infrastructure/tests/payment_contract_f418.rs, payment_port_contract.rs, payment_stub_isolation_f418.rs — suite runners plus AC1/AC2/AC4 guards (fail-closed URL check, adapter-name provenance, three deterministic fault scenarios).
- .github/workflows/ci.yml — `payment-stub` job (lines ~207–227): the blocking CI gate with adapter/fault-scenario env pinned.

Pre-existing types this extends: coxagent_application::ports::outbound (port registry); fault-attribution rules in crates/application/src/faults.rs consume `PaymentError::is_provider_fault()` at their existing seams.

## Related

- CXA-F418 ships on branch `feat/CXA-F418` (commit aace5e70) and was not merged into main at write time — verify before counting its jobs/files as present on main.
- SEC-8DED5F hardened the repo-wide `expect_used` lint story (`fdbe68b7`); the stub's file-scoped allow is the sanctioned interaction with it.
- Attribution semantics come from the failures/attempts work around crates/application/src/faults.rs (see the Failure Logs docs) — provider fault ≠ ticket fault is the invariant the error taxonomy exists for.
- Follows the same contract-suite shape as `sql_store_contract`/`kv_doc_contract` (see Deployment/configuration pages under wiki/engineering).
