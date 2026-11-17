# Payments sandbox stability: stub/live split + nightly runbook (CXA-F451)

**Keywords:** PaymentsPort, StubPayments, SandboxPaymentsClient, ScriptedFault,
payment sandbox, nightly, backoff, retry_after, idempotency, flaky tests

## Overview
PR CI runs every payments flow against a deterministic in-process stub; one
scheduled nightly job replays the same flows against the real sandbox with
alerting on failure. Root cause killed: shared sandbox credentials hammered by
parallelised integration tests with no backoff.

## The seam
- `application::ports::outbound::payments` — `PaymentsPort`
  (`charge`/`refund`), typed `PaymentError::{VendorUnavailable{status,retry_after_ms}, Refused(reason), Invalid{amount_minor,currency}}`.
  Any future billing feature MUST implement/bind this port — do NOT grow its own HTTP client.
- `infrastructure::payments::stub` — `StubPayments::new(HashMap<String, Vec<ScriptedFault>>)`,
  per-instance state, parallel-test safe, zero network.
- `infrastructure::payments::sandbox_client` — `SandboxPaymentsClient::new(base_url, api_key, max_retries, base_backoff_ms)`;
  exponential backoff + jitter honouring `retry-after`, fail-closed config validation naming the missing variable.
  Credentials only via composition-root injection; status codes and request ids are logged, never keys.

## Test split
| Suite | Where it runs | Network |
|---|---|---|
| `payments_stub_contract` (plain) | `check` job, <10 min total | none |
| `payments_sandbox_live` (`#[ignore]`) | nightly only, `--ignored --test-threads=1` | real sandbox |

Both cover: successful charge, declined card, timeout/vendor-unavailable,
duplicate/idempotency-retry.

## Nightly runbook (who gets paged)
1. `.github/workflows/nightly-live.yml` runs at 03:00 UTC (or manual dispatch).
2. On failure an issue labelled **`nightly-live-failure`** is opened/updated,
   naming the failing scenario(s) from the panic messages and linking the run.
3. A human owner picks it up the next working day — repeated alarms must change
   hands, not just re-fire. Distinguish sandbox-side breakage (rate limits,
   sandbox downtime) from stub/sandbox contract drift before touching code.
4. First-ever run: provision repo/org secrets `COXAGENT_SANDBOX_BASE_URL` +
   `COXAGENT_SANDBOX_API_KEY`. Until then the job exits with an explicit
   fail-closed message naming the missing variable — visible, not silent.

## Allowlist of known sandbox-side issues
Documented sandbox breakage is suppressed only via the ticket-tracked issue
label above, each entry carrying its expiry date — stale suppressions are
removed so a persistent non-sandbox failure still blocks merges to main.
