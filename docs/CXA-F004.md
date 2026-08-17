FOLDER: Engineering

# Playwright Golden Screenshot Suite

**Keywords:** Playwright, golden screenshots, visual regression, toHaveScreenshot, e2e specs, console-error gate, armConsoleGate, assertNoConsoleErrors, openApp, seed.mjs, run-server.sh

## Overview

The Playwright golden screenshot suite is CoXAgent's end-to-end UI guard. It boots a locally built debug binary against a frozen state fixture and drives every major view — overview KPI tiles and work board (`space.spec.ts`), chat (`chat.spec.ts`), wiki docs (`docs.spec.ts`), inbox/hybrid surfaces (`hybrid.spec.ts`) and the ticket dialogs (`ticket-dialog.spec.ts`, added by CXA-F004) — asserting seeded content and pixel-diffing each committed PNG golden. It is for any developer who changes rendering code (run it to prove pixels and module loads did not break) and for CI (`.github/workflows/visual-qa.yml`), which blocks PRs whose screenshots exceed tolerance.

CXA-F004 extends the suite begun by CXA-F002 so that **all major UI views are covered**. CXA-F002 shipped four specs (overview + board via `space.spec.ts`, chat via `chat.spec.ts`, docs via `docs.spec.ts`); later tickets added `sprint`, `cost`, `views`, `settings-config` and `hybrid`. F004 contributes exactly one new file — [`e2e/specs/ticket-dialog.spec.ts`](e2e/specs/ticket-dialog.spec.ts) — with two golden baselines: the new-ticket create form and the read dialog of a seeded ticket.

## How it works

Playwright launches CoXAgent itself through its webServer block instead of assuming a running hub:

1. **Boot.** [`run-server.sh <port>`](e2e/run-server.sh) requires a prebuilt binary at [`target/debug/coxagent`](../target/debug/coxagent); without one it prints "build first: cargo build --bin coxagent" to stderr and exits 1. When present it wipes `.state/`, copies [`fixtures/state/state.json`](e2e/fixtures/state/state.json) into throwaway `.state/serve`, deletes any parent-level `auth.json` residue from an earlier flat layout (which would otherwise switch RBAC on), unsets inherited DSN/admin env vars (`COXAGENT_DB_DSN`, `COXAGENT_AUTH_DSN`, `COXAGENT_REDIS_URL`, `COXAGENT_REMOTE_STORE_URL`, `COXAGENT_ADMIN_USER/PASSWORD`) so no live store leaks in, then runs:
   ```sh
   exec env COXAGENT_PORT="$PORT" "$BIN" --state-dir "$STATE" serve --work-dir "$HERE/.."
   ```
   The config pins port **4517** — never dogfood's port 4000.
2. **Seed.** [`seed.mjs`](e2e/seed.mjs) seeds deterministic content over the app's own HTTP API against `/api/projects/default`: three tickets (one carrying acceptance criteria), two chat messages ("Standup: timeline fix is in review", "Reminder: never bind port 4000..."), and one wiki page titled "Deploy health gate". Seeding through HTTP means fixtures can never drift from the state schema.
3. **Drive + assert.** Each spec uses shared helpers from [helpers.mjs](helpers.mjs): openApp(page) navigates to `/` then waits for network idle; real DOM interactions or injected window helpers reach each view; auto-retrying matchers confirm seeded text/elements.
4. **Pixel diff.** Committed goldens are matched by Playwright's toHaveScreenshot(); they live under `<spec>.spec.ts-snapshots/*-darwin.png`. Tolerance/stability settings live in [playwright.config.ts](playwright.config.ts).

The console-error gate is wired manually at the top of each test that needs it:

```ts
const errors = [];
armConsoleGate(page, errors);
await openApp(page);
// ... interactions ...
await assertNoConsoleErrors(errors);
```

armConsoleGate pushes every browser message of type 'error' plus every pageerror onto caller-supplied array; assertNoConsoleErrors fails with a quoted list if any entry exists.

CI (`.github/workflows/visual-qa.yml`) builds debug coxagent on ubuntu-latest Chromium against committed snapshots with regeneration disabled:

```sh
npx playwright test --update-snapshots=off --reporter=json > .playwright-report.json
```

It uploads `.playwright-report.json` plus per-test diff images as artifact `visual-qa-results-${{ github.sha }}`. On failure only it posts a PR comment listing each failing spec title.

## Usage

Build first; regenerate intentionally-changed goldens second; review them before committing.

```sh
cargo build --bin coxagent                 # required by run-server.sh -> target/debug/coxagent
cd e2e
npx playwright install chromium            # first time only
npm test                                   # = playwright test using e2e/playwright.config.ts
```

Run one spec file or one test title under e2e:

```sh
npx playwright test ticket-dialog          # filename substring under specs/
npx playwright test -g "new-ticket form"   # title regex across specs/
```

Regenerate goldens after an intentional UI change:

```sh
npm run baseline                           # = playwright test --update-snapshots
git status                                 # keep only intended *-darwin.png rewrites; revert others
```

CI-equivalent local check without regenerating:

```sh
npx playwright test --update-snapshots=off
```

Sample passing run for CXA-F004's file (test titles abbreviated):

```
$ npx playwright test ticket-dialog

Running 1 project using config at /path/to/repo/e2e/playwright.config.ts

  ✓ [chromium] › ticket-dialog › The new-ticket form renders every field (...ms)
  ✓ [chromium] › ticket-dialog › The read dialog shows a seeded ticket with its acceptance criteria (...ms)

  2 passed (...)
```

On failure CI exposes artifacts under `.playwright/test-results/<project>/test-failed-*/{actual|expected|diff}-*.png`.

