FOLDER: Testing
# Ticket Dialog Golden Screenshots (CXA-F004)

**Keywords:** golden screenshot, Playwright, toHaveScreenshot, new ticket form, ticket read dialog, acceptance criteria, console gate, e2e specs, darwin snapshot, window.openNewTicket, window.showTicket

## Overview

CXA-F004 adds deterministic golden-screenshot coverage for the two ticket dialogs in the CoXAgent web UI: the "new ticket" create form and the full read view of a seeded ticket. It is for anyone who changes the markup of either dialog (`#ov-newticket` or `#ov-ticket`) or breaks a JS module they load — such a change now shows up as a pixel diff or a console-error failure instead of slipping through an interaction-only test. Each spec is hermetic: it boots an ephemeral server on its own port against a frozen state fixture and asserts both pixels and a clean console.

## How it works

Both tests live in [`ticket-dialog.spec.ts`](../../../../e2e/specs/ticket-dialog.spec.ts) and share helpers from [`helpers.mjs`](../../../../e2e/specs/helpers.mjs):

1. `armConsoleGate(page, errors)` registers listeners that push any `console` error message or uncaught `pageerror` into an array.
2. `openApp(page)` navigates to `/` and waits for `networkidle`, so screenshots capture fully-painted content.
3. After opening each dialog and asserting its fields are present, the test calls `expect(page).toHaveScreenshot('<name>.png')`, then runs `assertNoConsoleErrors(errors)`, which fails the run if any JS error occurred — a refactor that keeps pixels identical but breaks a module load is caught by the gate even when the image matches.

The Playwright config ([`playwright.config.ts`](../../../../e2e/playwright.config.ts)) makes runs reproducible: fixed port **4517** (never the hub's 4000), viewport **1280×900**, dark color scheme with reduced motion set at project level (lines 21-24), animations disabled via screenshot options (line 17), workers=1 with retries=0. It starts itself via `webServer.command = sh ./run-server.sh 4517`. The frozen fixture under [`fixtures/state/`](../../../../e2e/fixtures/state/) provides deterministic tickets; as its own comment notes, frozen state still ages and relative timestamps render a few pixels differently run to run — this is why screenshot comparison allows `maxDiffPixelRatio: 0.02`.

```ts
// Canonical end-to-end shape shared by both specs:
const errors = [];
armConsoleGate(page);
await openApp(page);
await page.evaluate(() => window.openNewTicket());   // or showTicket('F001')
await expect(dlg).toBeVisible();
/* field assertions */
await expect(page).toHaveScreenshot('<name>.png');
assertNoConsoleErrors(errors);
```

## Usage

From repo root:

```bash
cargo build --bin coxagent        # required first; run-server.sh refuses if missing
cd e2e && npx playwright test      # run all specs
cd e2e && npx playwright test specs/ticket-dialog.spec.ts   # just these two
```

To refresh baselines after an intentional markup change:

```bash
cd e2e && npx playwright test --update-snapshots
```

The spec opens each dialog through its global entry point:

```ts
// New-ticket form:
await page.evaluate(() => window.openNewTicket());
const dlg = page.locator('#ov-newticket.open');
await expect(dlg).toBeVisible();
await expect(dlg.locator('#nt-ac')).toBeVisible();
await expect(page).toHaveScreenshot('new-ticket-form.png');

// Read view of seeded ticket F001:
await page.evaluate(() => window.showTicket('F001'));
const dlg = page.locator('#ov-ticket.open');
await expect(dlg).toBeVisible();
await expect(dlg.getByText('agents (pool)')).toBeVisible();
await expect(page).toHaveScreenshot('ticket-read.png');
```

## Interface

Globals invoked by the specs (both exposed on `window`, classic scripts sharing one scope):

| Function | Defined in | Opens |
|----------|-----------|-------|
| `window.openNewTicket()` | [shell.js](../../../../crates/presentation/src/web/js/shell.js) line ~250 | create-form overlay element id **`ov-newticket`**, given class **`.open`** |
| `window.showTicket(id)` | [chat.js](../../../../crates/presentation/src/web/js/chat.js) line ~1815 | read overlay element id **`ov-ticket`**, given class **`.open`** |

Selectors asserted by CXA-F004 — create form: option lists `#nt-type option` (hasText `feature`), `#nt-prio option` (hasText `medium`), `#nt-cx option` (hasText `medium`); textarea `#nt-ac` visible. Read view of seeded ticket F001: text "Search box scopes per tab", heading "Acceptance criteria", assignee row "agents (pool)", select `#tk-assign-sel` visible.

Screenshots produced (platform suffix appended automatically):

| Test name | Baseline file |
|-----------|---------------|
| "the new-ticket form renders every field" | [specs/ticket-dialog.spec.ts-snapshots/new-ticket-form-darwin.png](../../../../e2e/specs/ticket-dialog.spec.ts-snapshots/new-ticket-form-darwin.png) |
| "the read dialog shows a seeded ticket with its acceptance criteria" | [specs/ticket-dialog.spec.ts-snapshots/ticket-read-darwin.png](../../../../e2e/specs/ticket-dialog.spec.ts-snapshots/ticket-read-darwin.png) |

## Configuration

All behaviour comes from two files; there is no separate settings table for this suite.

[playwright.config.ts](../../../../e2e/playwright.config.ts):
- Port (**default 4517**) — constant at line 5; passed as `/run-server.sh ${PORT}`.
- Viewport (**1280×900**) — pinned UI geometry (line 22); changing it requires regenerating baselines.
- colorScheme (**dark**) / reducedMotion (**reduce**) (lines 23-24) and animations **disabled** via the screenshot options (line 17) — fixed visual state for stable pixels.
- maxDiffPixelRatio (**0.02**, line 16) — tolerates clock-driven drift while still catching real visual change.
- workers=1 / retries=0 / timeout 30_000 / reuseExistingServer=false (lines 10-12, and line 29) — serialized hermetic runs that never attach to another server.

[run-server.sh](../../../../e2e/run-server.sh), started by the config's `webServer.command`:
- Sets `COXAGENT_PORT=$PORT` so dogfood builds never bind the hub's port **4000**.
- Wipes `.state/serve` then copies `fixtures/state` into it fresh on every run, so RBAC stays off and screenshots stay deterministic; also clears a stale legacy `auth.json`.
- Unsets `COXAGENT_DB_DSN`, `COXAGENT_AUTH_DSN`, `COXAGENT_REDIS_URL`, `COXAGENT_REMOTE_STORE_URL`, `COXAGENT_ADMIN_USER`, and `COXAGENT_ADMIN_PASSWORD` so an operator's exported environment cannot flip RBAC on or point the fixture at shared state.

## Edge cases and limits

What CXA-F004 deliberately does NOT do:

- It does not cover authentication or RBAC flows. Those have their own parallel suite under [`specs-auth/`](../../../../e2e/specs-auth/) booted by [`run-server-auth.sh`](../../../../e2e/run-server-auth.sh).
- The golden snapshots are platform-scoped: Playwright appends a suffix (`darwin`) per host OS, so baselines checked in on one platform do not match on another without regeneration.
- Only these two dialogs have golden coverage. The other major views (overview KPIs, activity, settings) are smoke-tested for console errors only in [`views.spec.ts`](../../../../e2e/specs/views.spec.ts); they have no image baselines yet.
- Pixel comparison allows up to **2%** of differing pixels (`maxDiffPixelRatio: 0.02`) because rendered relative timestamps drift between runs; genuinely large layout changes still fail, but small text-time drift does not.
- Failure modes: missing dialog element fails the visibility assertion before any screenshot is taken; console/module errors fail via the gate even if pixels match; a changed viewport or color scheme invalidates baselines silently until they are regenerated.

## Code map

Every file that implements or directly supports CXA-F004:

— [`docs/wiki/engineering/testing/cxa-f004-ticket-dialog-golden-screenshots.md`](./cxa-f004-ticket-dialog-golden-screenshots.md) — this page.
— [`crates/presentation/src/web/js/shell.js:250`](../../../../crates/presentation/src/web/js/shell.js) — defines `window.openNewTicket()`, which renders and opens the create-form overlay id **`ov-newticket`** (plus its sibling create-submit flow).
— [`crates/presentation/src/web/js/chat.js:1815`](../../../../crates/presentation/src/web/js/chat.js) — defines `window.showTicket(id)`, which renders and opens the read overlay id **`ov-ticket`** with assignee select **:tk-assign-sel** and acceptance-criteria section.
— [`e2e/specs/helpers.mjs`](../../../../e2e/specs/helpers.mjs) — `armConsoleGate`, `assertNoConsoleErrors`, and `openApp`, shared by every spec.
— [`crates/presentation/src/web/js/inbox.js`](../../../../crates/presentation/src/web/js/inbox.js), [`home.js`](../../../../crates/presentation/src/web/js/home.js), [`core.js`](../../../../crates/presentation/src/web/js/core.js) — other callers of `showTicket(...)`; changing its signature affects these, so run impact analysis before editing it.
— [`e2e/fixtures/state/`](../../../../e2e/fixtures/state/) — frozen JSON state snapshotted into `.state/serve` each run; provides seeded ticket F001 ("Search box scopes per tab") that the read-view baseline depends on.

## Related

- CXA-F002 "Expand Playwright golden screenshot specs to cover all major UI views" (merged just before this branch's own expansion) — the origin of the console-error-gate + toHaveScreenshot pattern reused here.
- [views.spec.ts](../../../../e2e/specs/views.spec.ts) — smoke coverage of overview KPIs, activity, and settings views without image baselines; a natural next target for more goldens.
- [specs-auth / run-server-auth.sh](../../../../e2e/specs-auth/) — the parallel authentication/RBAC suite (login, validation, rbac-viewer, guest-protected, auth-gate) with its own server bootstrap; intentionally out of scope for CXA-F004.

