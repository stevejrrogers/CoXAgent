FOLDER: Application Ports
# Outbound Email Port and Stub Mailer (CXA-F444)

**Keywords:** email, EmailPort, StubMailer, outbox, port, adapter, conformance test, hexagonal, email address validation, failure injection

## Overview
Defines the outbound transactional-email capability (invites, password resets, deploy alerts) as a driven port in the application layer, plus a network-free stub adapter so consumer flows can run end-to-end offline today. QA and operators audit what "left" via an in-memory captured-outbox; a real SMTP/vendor adapter is a follow-up that must satisfy the same conformance suite.

## How it works
1. A use case builds an `EmailMessage` with the guarded constructor `EmailMessage::new(...)` — empty recipient lists are rejected (`EmailError::Rejected`) at construction time.
2. Recipient/sender addresses go through `EmailAddress::try_from(&str)`, which enforces RFC 5321 limits inline (≤254 total chars, ≤64-char local part, exactly one `@`, no whitespace). Invalid states are unrepresentable — validation lives in the type, not call sites.
3. The use case holds an `Arc<dyn EmailPort>` and calls `send(message)` → `Result<EmailReceipt, EmailError>`. Every send returns either a receipt or a typed error; it never hangs silently or panics.
4. The only current implementation, `StubMailer`, resolves its `FailureScript` first — failed sends never reach the outbox — then assigns a monotonic id `stub-email-<n>`, stamps `accepted_at_utc` from the caller-injected clock closure, logs via `tracing::info!`, and appends to the shared in-process outbox.
5. Observability is a separate read-back trait, `EmailOutboxPort`; on the stub you get `StubMailer::outbox_snapshot()` returning accepted messages oldest-first.

## Usage
```rust
use std::sync::Arc;
use coxagent_application::ports::outbound::{EmailAddress, EmailMessage};
use coxagent_infrastructure::{FailureScript, StubMailer};

let mailer = Arc::new(StubMailer::new(|| FIXED_CLOCK.to_string())); // injected clock
let port: Arc<dyn EmailPort> = mailer.clone();

let from = EmailAddress::try_from("noreply@coxagent.dev")?;
let to = vec![EmailAddress::try_from("alice@example.com")?];
let msg = EmailMessage::new(from, to, "Reset your password", "Click https://… within 30 min.")?;

let receipt = port.send(msg).await?;               // message_id: "stub-email-1"
let outbox = mailer.outbox_snapshot();             // full audit trail, in send order

// reliability testing without a vendor sandbox:
let flaky = StubMailer::new(|| FIXED_CLOCK.to_string())
    .with_failure_script(FailureScript::FailAfter(2)); // then TemporarilyUnavailable forever
```
Run the contract suite: `cargo test -p coxagent-infrastructure --test email_port_conformance`.

## Interface
Application-layer types (`coxagent_application::ports::outbound`):
- `trait EmailPort { async fn send(&self, message: EmailMessage) -> Result<EmailReceipt, EmailError>; }`
- `trait EmailOutboxPort { fn outbox() -> Vec<CapturedEmail>; }`
- `struct EmailAddress(String)` — `TryFrom<&str>`, `Display`, `AsRef<str>`
- `struct EmailMessage { from, to: Vec<EmailAddress>, subject, text_body }` + `EmailMessage::new(...)`
- `struct EmailReceipt { message_id: String, accepted_at_utc: String }`
- `enum EmailError { InvalidAddress(String), Rejected(String), TemporarilyUnavailable }`
- `struct CapturedEmail { message_id, to, subject, text_body, accepted_at_utc }`

Infrastructure adapter (`coxagent_infrastructure`):
- `StubMailer::new(clock: impl Fn() -> String) -> Self` (Clone; cheap clones share the outbox)
- `StubMailer::with_failure_script(self, FailureScript) -> Self`
- `StubMailer::outbox_snapshot() -> Vec<CapturedEmail>`
- `enum FailureScript { Never, FailAfter(u32), AlwaysFail(EmailError) }` (default `Never`)

## Configuration
No runtime configuration. Two composition-root choices control behaviour:
- **Clock**: passed to `StubMailer::new(...)` — inject a constant RFC3339 UTC string in tests for exact assertions, a real clock in production wiring. There is no default.
- **FailureScript**: default `Never` (all sends succeed); set explicitly via `with_failure_script` when testing vendor flap.

## Edge cases and limits
- **Deliberately no real delivery**: sockets, secrets and retries live in the future vendor adapter; the stub is purely in-process and offline by construction.
- **The outbox is an attribution record, not a retry log**: failed sends are absent from snapshots; there is no history of failures.
- **Not durable**: process-local memory (`Mutex<Vec<_>>`); cloning `StubMailer` shares the same outbox, but restarting loses everything.
- **Invalid addresses are data errors** (`InvalidAddress` with reason), provider outage is `TemporarilyUnavailable` — callers distinguish "fix the data" from "retry later"; the port itself does not retry.
- **Empty recipient list fails fast** at `EmailMessage::new`, so "silently sent to nobody" cannot exist.
- It does NOT do HTML bodies, attachments, headers or Bcc/Cc — plain-text, To-only per `EmailMessage`.

## Code map
- crates/application/src/ports/outbound/email.rs — the port: `EmailAddress` validation, `EmailMessage`, `EmailReceipt`, `EmailError`, `EmailPort`, `EmailOutboxPort`, `CapturedEmail` + unit tests
- crates/application/src/ports/outbound/mod.rs — module registration and re-export of the email port types
- crates/infrastructure/src/email_stub.rs — `StubMailer`, `FailureScript`, shared-outbox capture logic + unit tests
- crates/infrastructure/tests/email_port_conformance.rs — adapter-agnostic contract suite exercised through `Arc<dyn EmailPort>`, reused verbatim by any future real adapter

## Related
- wiki/product/configuration-coverage-and-config-drift-gate.md — how the sender identity is configured at the composition root once real providers land
- ARCHITECTURE.md — hexagonal ports-and-adapters rules this ticket follows (IO behind a port, pure decision logic inward)
- Ticket CXA-F444 — original scope: port contract types + network-free `StubMailer` with captured-outbox audit trail
