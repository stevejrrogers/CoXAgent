FOLDER: Visual QA
# Playwright Golden Screenshot Suite (CXA-F004)

**Keywords:** playwright, golden screenshot, visual regression, toHaveScreenshot, e2e specs, console-error gate, armConsoleGate, assertNoConsoleErrors, openApp, ticket dialog spec, linux snapshots

## Overview

CXA-F004 expands CoXAgent's Playwright golden-screenshot coverage so every major UI view has a pixel baseline plus a console-error gate — continuing from CXA-F002 which shipped the first four view goldens. It is for any developer who changes web UI markup or JS rendering: a change that shifts pixels or breaks a module load fails here before it reaches reviewers or merges to protected `main`. The suite boots an ephemeral CoXAgent server against frozen fixture state so screenshots are deterministic across runs and hosts.

CXA-F004 lands on branch `feat/CXA-F004` as two commits: `755f40e` adds exactly one new spec file (`e2e/specs/ticket-dialog.spec.ts`) with two new golden baselines covering the two ticket dialogs that previously had none; `f836028` ("fix(CXA-B064)") makes those baselines reproducible across macOS and Linux CI by pinning host-dependent responses and checking in Linux snapshots alongside macOS ones.

## How it works

Every spec follows one shape built on shared helpers in [`e2e/specs/helpers.mjs`](../../e2e/specs/helpers.mjs):

1. **Boot.** [`playwright.config.ts`](../../e2e/playwright.config.ts) boots an ephemeral server itself via `webServer.command = sh ./run-server.sh 4517`; [`run-server.sh`](../../e2e/run-server.sh) requires a prebuilt binary at `target/debug/coxagent`, wipes `.state`, copies [`fixtures/state/*`](../../e2e/fixtures/state/) into throwaway `.state/serve`, and unsets DB/auth DSNs so RBAC stays off.
2. **Deterministic engine.** As part of F004's cross-platform fix, `openApp(page)` routes `/api/engines` and returns one pinned engine (`opencode`) so a bare runner with no agent CLIs never pops the "Connect a coding agent" modal over later clicks; pixels are identical on darwin and linux.
3. **Drive + assert.** Each test arms the console gate — `armConsoleGate(page, errors)` collects every browser message of type `error` plus each uncaught `pageerror`. Navigation uses real clicks or injected window helpers (`window.openNewTicket()`, `window.showTicket('F001')`) to reach its view.
4. **Pixel diff.** The test calls `expect(page).toHaveScreenshot('<name>.png')`, then closes with `assertNoConsoleErrors(errors)` — failing if even one JS error occurred even when pixels match.
5. **Report / gate.** The Visual QA workflow (`.github/workflows/visual-qa.yml`) runs this suite on PR/push; its job display name ("Visual QA", spaces intact) is itself the required status check protected main gates merges on.

The canonical end-to-end shape:

```ts
const errors = [];
armConsoleGate(page, errors);
await openApp(page);
// ... interactions / await page.evaluate(() => window.openNewTicket())
await expect(dlg).toBeVisible();
await expect(page).toHaveScreenshot('<name>.png');
assertNoConsoleErrors(errors);
```

## Usage

From repo root:

```bash
cargo build --bin coxagent                  # required first; run-server.sh refuses if missing
cd e2e && npx playwright install chromium  # first time only
cd e2e && npx playwright test               # whole suite
cd e2e && npx playwright test ticket-dialog # just CXA-F004's file (filename substring)
cd e2e && npx playwright test -g "new-ticket form"  # by title regex
```

Regenerate baselines after an intentional visual change:

```bash
cd e2e && npx playwright test --update-snapshots
git status  # keep only intended *-darwin.png / *-linux.png rewrites; revert others
```

CI-equivalent local check that refuses to rewrite baselines:

```bash
cd e2e && npx playwright test --update-snapshots=off
```

Sample run for CXA-F004's file:

```
$ cd e2e && npx playwright test ticket-dialog

Running 1 project using config at /path/to/repo/e2e/playwright.config.ts

  ✓ [chromium] › ticket-dialog › The new-ticket form renders every field (...ms)
  ✓ [chromium] › ticket-dialog › The read dialog shows a seeded ticket with its acceptance criteria (...ms)

  2 passed (...)
```

On failure Playwright writes diffs under `.playwright/test-results/<project>/test-failed-*/{actual|expected|diff}-*.png`.

## Interface

View coverage map — which spec owns which view after CXA-F004 (golden names omit the OS suffix):

| View | Spec file | Golden |
|---|---|---|
| Overview KPI tiles + Ticket-status block | `e2e/specs/space.spec.ts` | overview |
| Work board (seeded tickets) | `e2e/specs/space.spec.ts` | board |
| Chat mode + scoped search | `e2e/specs/chat.spec.ts` | chat |
| Wiki docs page read | `e2e/specs/docs.spec.ts` | docs |
| Inbox empty state + hidden nav badge | `e2e/specs/hybrid.spec.ts` | inbox-empty |
| New-ticket create form (**CXA-F004**) | `e2e/specs/ticket-dialog.spec.ts` | new-ticket-form |
| Ticket read dialog (**CXA-F004**) | `e2e/specs/ticket-dialog.spec.ts` | ticket-read |

Baselines live in per-spec directories `<spec>.ts-snapshots/*.<platform>.png`, e.g. `ticket-dialog.spec.ts-snapshots/new-ticket-form-darwin.png`. Playwright appends the platform suffix automatically from the host OS (`darwin`, or `linux`) — see Edge cases.

Globals invoked by CXA-F004's two tests (plain top-level function declarations, reachable from a classic single-scope script):

- **openNewTicket()** — defined in [`shell.js:250`](../../crates/presentation/src/web/js/shell.js); clears title/ac/ui flags then reveals overlay id **ov-newticket** by adding class `.open`.
- **showTicket(id)** — defined in [`chat.js:1815`](../../crates/presentation/src/web/js/chat.js); fetches `/api/tickets/{id}` (falling back to local state), renders into container id **ticket-body** inside overlay id **ov-ticket**.

Selectors asserted by CXA-F004's two tests:

| Selector / text | Meaning |
|---|---|
| `#ov-newticket.open` | New-ticket create-form overlay revealed (class `.open`) |
| `#nt-type option` hasText `feature`, `#nt-prio`/`#nt-cx` option hasText `medium`, `#nt-ac` visible | Create-form fields present with seeded defaults |
| `#ov-ticket.open .modal #ticket-body` | Read-dialog overlay + body container |
| text "Search box scopes per tab", "Acceptance criteria" | Seeded ticket F001 content rendered in read dialog |
| text "agents (pool)", selector `#tk-assign-sel` | Unassigned assignee row + its select |

Overlay markup lives in [`crates/presentation/src/web/index.html:679 (#ov-ticket)`](../../crates/presentation/src/web/index.html) and [`index.html:688 (#ov-newticket)`](../../crates/presentation/src/web/index.html).

## Configuration

All behaviour comes from `e2e/playwright.config.ts` plus the two F004 additions that pin host-dependent responses; there is no separate settings table.

[`playwright.config.ts`](../../e2e/playwright.config.ts):
- Port **4517** (constant, line 5) — passed to `run-server.sh`, never the hub's 4000.
- Viewport **1280×900**, colorScheme **dark**, reducedMotion **reduce** — pinned visual state for stable pixels.
- Screenshot tolerance `maxDiffPixelRatio: 0.02` and `animations: 'disabled'` — absorbs clock-driven timestamp drift while still catching real layout changes.
- `fullyParallel: false`, `workers: 1`, `retries: 0`, timeout **30_000** — serialized hermetic runs against one server.

F004 cross-platform pins (commit f836028), both via `page.route`:
- In [`helpers.mjs::openApp(page)`](../../e2e/specs/helpers.mjs): routes **GET /api/engines** → returns one pinned engine (`opencode`) so bare runners without agent CLIs do not pop the "Connect a coding agent" modal over later clicks.
- In [`cost.spec.ts`](../../e2e/specs/cost.spec.ts): routes **GET /api/token-saver** → a fixed JSON body so the token-saver panel renders identically on hosts with or without leftover shim state.

## Edge cases and limits

What CXA-F004 deliberately does NOT do:

- **It does not add goldens for every nav view.** Views still covered only by console-gate smoke tests (no image baseline) include Activity, Settings (`views.spec.ts`), and the Cost/insights panels (`cost.spec.ts`). "All major UI views" means the primary data-display surfaces plus both ticket dialogs; auxiliary/admin surfaces remain smoke-only.
- **It does not cover auth/RBAC flows.** Those live in a parallel suite under `e2e/specs-auth/`, booted by `run-server-auth.sh`.
- **No behaviour assertions here.** CXA-F004 asserts field presence + pixels + clean console; save/cancel mutation behaviour belongs to functional specs like `tickets.spec.ts`.

How it fails / known limits:

- **Platform-scoped baselines.** Playwright appends an OS suffix (`darwin`, or `linux`) to each golden. A checked-in baseline matches only its own platform — which is exactly why F004 shipped both `*-darwin.png` and `*-linux.png`. The Visual QA workflow therefore runs on `macos-latest`.
- **Any single console error or JS exception fails** via the gate even when pixels match — a refactor that keeps pixels but breaks a module load trips it.
- **Clock drift on frozen state.** Rendered relative timestamps drift a pixel or two between runs; the 0.02 maxDiffPixelRatio absorbs this but is not absolute — a real layout shift still trips it.
- **Stale/missing build aborts at startup** (`build first: cargo build --bin coxagent`), surfacing as an infrastructure failure rather than an assertion failure.

## Code map

Files that implement or directly support CXA-F004:

- `e2e/specs/ticket-dialog.spec.ts` — CXA-F004's two golden tests: the new-ticket create form (`openNewTicket()`) and the read dialog of seeded ticket F001 (`showTicket('F001')`); each asserts DOM then screenshots behind the console gate. (Lives on branch `feat/CXA-F004`, not yet on `main`.)
- `e2e/specs/ticket-dialog.spec.ts-snapshots/new-ticket-form-darwin.png`, `new-ticket-form-linux.png` — goldens for the create form.
- `e2e/specs/ticket-dialog.spec.ts-snapshots/ticket-read-darwin.png`, `ticket-read-linux.png` — goldens for the read dialog.
- `e2e/specs/helpers.mjs` — shared `armConsoleGate` / `assertNoConsoleErrors` / `openApp`; F004 added the `/api/engines` route pin inside `openApp`.
- `e2e/specs/cost.spec.ts` — F004 added the `/api/token-saver` route pin so its panels render identically across hosts.
- The other specs' snapshot dirs gained Linux variants under F004: `chat`, `space/{overview,board}`, `hybrid/inbox-empty`, and their existing darwin baselines.
- `.github/workflows/visual-qa.yml` — runs this suite on macOS; its "Visual QA" job display name is the required status check protected main gates merges on.
- [`crates/presentation/src/web/js/shell.js:250`](../../crates/presentation/src/web/js/shell.js) — defines `window.openNewTicket()`.
- [`crates/presentation/src/web/js/chat.js:1815`](../../crates/presentation/src/web/js/chat.js) — defines `window.showTicket(id)`.
- [`crates/presentation/src/web/index.html:679,688`](../../crates/presentation/src/web/index.html) — markup for overlays **ov-ticket** and **ov-newticket**.
- `e2e/fixtures/state/state.json` — frozen JSON state that `run-server.sh` copies into throwaway `.state/serve` each run; it carries seeded ticket F001 ("Search box scopes per tab") plus chat messages and a wiki page, which is what makes screenshot pixels deterministic.
- [`crates/app/tests/branch_protection_gate.rs`](../../crates/app/tests/branch_protection_gate.rs) — added by f836028; guards the branch-protection setup so the required "Visual QA" check cannot silently go missing again.

## Related

- **CXA-F002** "Expand Playwright golden screenshot specs to cover all major UI views" — the parent effort this ticket continues; shipped the first four view goldens (overview, board, chat, docs).
- [Visual QA merge gate + auto-ticketing (CXA-F005/F006)](../../docs/wiki/testing/cxa-f006-visual-qa-merge-gate.md) — takes this suite up into CI-side regression gating and auto-filed Visual QA bugs.
- Sibling page [Ticket Dialog Golden Screenshots](../engineering/ticket-dialog-golden.md) documents these same two dialogs from a narrower angle.
