FOLDER: Playwright Golden Screenshots

# Ticket Dialog Golden Screenshots (CXA-F004)

**Keywords:** playwright, golden screenshot, toHaveScreenshot, ticket dialog, new-ticket form, ticket read view, ov-newticket, ov-ticket, openNewTicket, showTicket, console-error gate, e2e fixture state

## Overview

CXA-F004 adds Playwright golden (pixel-baseline) screenshot specs for the two ticket dialogs — the new-ticket create form and the full read view of a seeded ticket — extending an F-series effort that covers every major UI view with screenshots plus a console-error gate. It is for anyone changing web UI markup or JS rendering: a change that shifts pixels or breaks a module load fails here before it reaches reviewers. The specs run against an ephemeral server booted from frozen fixture state so pixels are deterministic.

## How it works

Each spec is a classic single-scope Playwright test under `e2e/specs/`. The shared helpers in `helpers.mjs` (`armConsoleGate`, `assertNoConsoleErrors`, `openApp`) arm a console/pageerror listener that fails any test on even one console error or JS exception — so a refactor that keeps pixels but breaks module load still trips the gate. `openApp` navigates to `/` and waits for `networkidle`.

For CXA-F004's two tests:

1. **new-ticket form** (`#ov-newticket`) — drives the global function `openNewTicket()` (defined in shell.js) via `page.evaluate`, which resets fields and adds class `.open` to reveal `<div class="ov" id="ov-newticket">`. The test asserts each field's presence (`#nt-type option[hasText=feature]`, default priority/complexity `medium`, acceptance-criteria textarea `#nt-ac`), then calls `expect(page).toHaveScreenshot('new-ticket-form.png')`.
2. **read dialog** (`#ov-ticket`) — drives global `showTicket('F001')` (chat.js), which fetches `/api/tickets/F001` (falling back to local state on failure) and renders title/subject/meta into `<div class="modal" id="ticket-body">`. Assertions cover seeded content ("Search box scopes per tab", "Acceptance criteria"), the unassigned assignee row ("agents (pool)" + selector `#tk-assign-sel`), then snapshots as 'ticket-read.png'.

Snapshot files live under per-spec directories named `<spec>.ts-snapshots/` with an OS suffix (`new-ticket-form-darwin.png`, `ticket-read-darwin.png`) added automatically by Playwright from our macOS platform.

Playwright boots its own CoXAgent server via e2e/playwright.config.ts (`webServer.command = sh ./run-server.sh <PORT>`); run-server.sh copies frozen fixture state from e2e/fixtures/state into a throwaway dir on port 4517 and deliberately unsets DB/auth DSNs so no login wall appears.

## Usage

The suite runs from inside the e2e directory against a locally built binary:

```bash
cd e2e && npx playwright test
```

Run just this spec:

```bash
cd e2e && npx playwright test ticket-dialog
```

Update golden baselines after an intentional visual change (review diffs carefully):

```bash
cd e2e && npx playwright test ticket-dialog --update-snapshots
```

Prerequisites: build first with `cargo build --bin coxagent`; run-server.sh fails fast if that binary is missing.

## Interface

Global JS functions under test (all defined in crates/presentation/src/web/js/):

- openNewTicket() — shell.js: clears title/ac/ui flags then reveals dialog by adding class `.open`
- saveTicket(startFlow) — shell.js: POSTs `/api/tickets`, optionally POSTs `/api/control/resume`, refetches `/api/state`
- showTicket(id) — chat.js: fetches `/api/tickets/{id}`, renders into element id "ticket-body"

DOM identifiers asserted in specs:

| Selector | Meaning |
|---|---|
| #ov-newticket + .open | New-ticket modal overlay revealed |
| #nt-title / #nt-desc / #nt-type / #nt-prio / #nt-cx / #nt-ui / #nt-ac | New-ticket form fields |
| #ov-ticket + .open / #ticket-body | Read-dialog overlay + body container |
| #tk-assign-sel | Assignee select shown when no assignee |

Shared spec helpers exported by helpers.mjs:

- armConsoleGate(page, errors): wires page.on('console')/'pageerror' collecting failures into array
- assertNoConsoleErrors(errors): expects collected array empty else fails listing them
- openApp(page): goto('/') then waitForLoadState('networkidle')

API endpoints exercised indirectly by these dialogs:

- GET `/api/tickets/{id}` — full detail incl. design/test-case payload used by showTicket; falls back to local STATE.tickets on fetch failure.
- POST `/api/tickets` — create path behind saveTicket; not asserted by this spec's read of seeded data.

## Configuration

Behaviour is configured almost entirely through files rather than flags:

| Setting | Location | Default |
|---|---|---|
| Test port + baseURL | e2e/playwright.config.ts → use.baseURL & webServer url | http://127.0.0.1:4517 |
| Viewport width × height | use.viewport | 1280 × 900 |
| Color scheme for snapshots | use.colorScheme | dark |
| Reduced motion forced on for tests+snapshots | use.reducedMotion 'reduce'; expect.toHaveScreenshot animations 'disabled' | reduce / disabled |
| Snapshot pixel tolerance due to timestamp drift on frozen state | expect.toHaveScreenshot.maxDiffPixelRatio | 0.02 |
| Serial execution of specs over one server instance | fullyParallel false, workers 1, retries 0, timeout 30000 ms | serial / no retries |

Fixture determinism is structural rather than configurable: run-server.sh wipes `$HERE/.state` on every invocation, so seeded tickets never accumulate across runs and screenshots stay reproducible. No env or CLI flag controls this.

## Edge cases and limits

What CXA-F004 deliberately does **not** do:

- It does not assert dialog *behaviour* (save/cancel mutation); that belongs to the functional `tickets.spec.ts`, which already covers cancel-without-mutation. The F004 spec asserts field presence + pixels + no console errors.
- It does not cover auth-gated flows — login/RBAC views live under e2e/specs-auth with their own helpers and server script (run-server-auth.sh).
- Snapshots are platform-specific: baselines carry a `darwin` suffix, so a different OS will fail `toHaveScreenshot` until baselines for that platform are generated.

How it fails / known limits:

- Any single console error or JS exception fails the test via the gate even if pixels match.
- Golden diffs can be noisy on frozen state because relative timestamps drift a pixel or two; the 0.02 maxDiffPixelRatio tolerance absorbs this but is not absolute — a real layout shift still trips it.
- A stale/missing build aborts at startup (`build first: cargo build --bin coxagent`), surfacing as an infrastructure failure rather than an assertion failure.
- On machines that previously ran the old flat-state layout, a leftover top-level auth.json enables RBAC and turns unauthenticated specs into login-wall timeouts; run-server.sh deletes it each run.

## Code map

- e2e/specs/ticket-dialog.spec.ts — CXA-F004's two golden tests (new-ticket form fields + read dialog of seeded F001), each asserting DOM then taking a screenshot behind the console-error gate.
- e2e/specs/ticket-dialog.spec.ts-snapshots/new-ticket-form-darwin.png — golden baseline for the create-form screenshot.
- e2e/specs/ticket-dialog.spec.ts-snapshots/ticket-read-darwin.png — golden baseline for the read-dialog screenshot.
- e2e/specs/helpers.mjs — shared armConsoleGate / assertNoConsoleErrors / openApp used by every spec incl. F004.
- e2e/fixtures/state/state.json — frozen fixture carrying ticket F001 ("Search box scopes per tab") plus B001/F002/Chat/Docs entries served to the specs.
- e2e/playwright.config.ts — boots ephemeral server on port 4517, dark scheme, viewport 1280×900, snapshot tolerance/animation settings, serial single-worker execution.
- e2e/run-server.sh — copies fixture state into throwaway `.state`, unsets DB/auth DSNs/admin creds to keep RBAC off, execs `target/debug/coxagent serve`.
- crates/presentation/src/web/js/shell.js — defines openNewTicket() (form reveal) and saveTicket() (POST `/api/tickets`, optional resume).
- crates/presentation/src/web/js/chat.js — defines showTicket(id): fetch detail from `/api/tickets/{id}`, render into #ticket-body with assignee select (#tk-assign-sel), acceptance criteria list, design attachments section.
- crates/presentation/src/web/index.html — markup for `#ov-newticket` modal (lines ~688–722) and `#ov-ticket` overlay (line ~679).

## Related

- CXA-F002/CXA-F003 — sibling F-series tickets expanding golden coverage across other UI views (chat/docs/hybrid inbox/space board+overview).
- e2e/specs/tickets.spec.ts — functional new-ticket dialog test (fields + cancel-without-mutate); complements F004's visual-only coverage of the same form.
- e2e/specs-auth/ — auth-gated view coverage with its own helpers and run-server-auth.sh; connected through AGENTS.md's rule that any UI change must pass `cd e2e && npx playwright test`.

