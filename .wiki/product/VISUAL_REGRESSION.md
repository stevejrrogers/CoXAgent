FOLDER: Product

# Visual Regression

**Keywords:** visual regression, screenshots, Playwright, GitHub Actions, PR check, toHaveScreenshot, golden snapshots, pixel diff, 2% tolerance

## Overview

CXA-F005 adds a GitHub Actions workflow that auto-runs the e2e screenshot suite on pull requests and reports pixel-level diffs back to the PR thread. It exists to catch unintended changes in the classic (non-framework) web UI — `web/app.css` + `web/js/*.js`, one shared JS scope where load order matters — before merge. Each spec captures screenshots against committed golden `.png` baselines with a 2% pixel-diff tolerance plus a console-error gate.

**Status: scaffolded but DISABLED.** The job body in `.github/workflows/visual-qa.yml` is fully written but gated off with `if: false`, so it does not run on PRs today. Landing CXA-F005 means removing that gate and confirming the suite passes in CI (see Edge cases). This page documents the intended design exactly as it stands in code.

## How it works

One GitHub Actions job (`visual-regression`) drives the existing Playwright suites:

1. Trigger: `pull_request` events of type `opened` / `synchronize`.
2. Steps: checkout → dtolnay/rust-toolchain@stable → Swatinem/rust-cache@v2.
3. Build the debug binary with `cargo build --bin coxagent`. Required because `e2e/playwright.config.ts` sets its webServer command to run that binary; if absent, server startup fails (`run-server.sh` prints "build first" and exits 1).
4. Install headless Chromium + system deps with `npx playwright install chromium --with-deps`.
5. Run the visual suite from `e2e/`, snapshots OFF:
   ```
   npx playwright test --update-snapshots=off --reporter=json
   ```
   With updates off, any screenshot differing from its baseline beyond tolerance fails.
6. Per view each spec navigates via locators/text clicks (e.g. `nav('docs')`, mode buttons), asserts seeded content is present (`expect(page.locator('body')).toContainText(...)`), asserts zero console errors via helpers (`armConsoleGate` / `assertNoConsoleErrors`), then calls `expect(page).toHaveScreenshot('name.png', { fullPage })`. Baselines live next to each spec under `<spec>.ts-snapshots/<name>-darwin.png`.
7. The JSON report is written to `.playwright-report.json`. On failure an inline GitHub Script action parses it — walking nested arrays suites→specs→tests→results for statuses equal to `'failed'` — counts them ("N test(s) exceeded the 2% screenshot tolerance") and posts an issue comment listing failing spec titles plus a pointer to run artifacts.
8. Artifacts are always uploaded via actions/upload-artifact@v4 from paths including `e2e/test-results/`.

The server each spec runs against boots an isolated copy of frozen fixture state on port **4517** (auth variants use **4518**), never hub port **4000**, and unsets ambient Postgres/Redis/admin DSN env vars so no real or shared state leaks into screenshots (`run-server.sh`, AGENTS.md "Running the app").

## Usage

Generate or refresh golden baselines locally:

```
cargo build --bin coxagent
cd e2e
npm run baseline            # == playwright test --update-snapshots
```

Run only the comparison without touching baselines:

```
cd e2e
npx playwright test         # all specs; fails on > 0.02 pixel ratio diff
```

Run one view for a quick local result:

```
cd e2e && npx playwright test docs.spec.ts --reporter=list
```

Expected pass output per spec: content assertions green, no console errors, screenshots matched within tolerance.

When enabled in CI, opening any PR shows a "Visual QA" check; on failure it auto-posts a comment like:

> ## Visual QA failed
> 1 test(s) exceeded the 2% screenshot tolerance.
> Failing spec(s):
> - <failing spec title>

Diff images are downloadable from that workflow run's artifacts named `visual-qa-results-<sha>`.

After an intentional UI change you must commit new baselines (via baseline mode above); snapshots are part of source control or CI will fail.

## Interface

Workflow — `.github/workflows/visual-qa.yml`

| Field | Value |
|---|---|
| Workflow name | Visual QA |
| Trigger | pull_request types [opened, synchronize] |
| Permissions | contents read · issues write · pull-requests write |
| Job id / label | visual-regression / "Visual regression (screenshots)" |
| Runner / timeout | ubuntu-latest / timeout-minutes 30 |
| Job gate | disabled by conditional guard |

The whole job is currently disabled by its own top-level condition:
```yaml
jobs:
  visual-regression:
    if: false          # disables this job entirely right now
    ...
```
Removing/disabling this line re-enables automatic execution on PRs.

Playwright matcher used by every visual spec:
```js
await expect(page).toHaveScreenshot('name.png', { fullPage });
```

Global screenshot expectation defaults (`e2e/playwright.config.ts`, non-auth suite):
```js
expect: {
  toHaveScreenshot: {
    maxDiffPixelRatio: 0.02,
    animations: 'disabled',
  },
}
```

CLI flags used by CI (beyond config defaults):
```
--update-snapshots=off     # never rewrite committed goldens during CI compare
--reporter=json            # machine-readable report parsed by post-comment step
```

Two Playwright configs define two suites:
- Non-auth visuals — port **4517**, specs in `specs/`.
- Auth visuals — port **4518**, specs in `specs-auth/`, own config; covers RBAC/login rather than pure screenshots.

## Configuration

Settings that change capture behaviour, with their defaults as read from both Playwright configs:

| Setting | Where (as declared) | Default |
|---|---|---|
| Pixel diff tolerance above which a screenshot fails | `expect.toHaveScreenshot.maxDiffPixelRatio` — non-auth config; auth config omits it (Playwright's own default applies there) | `0.02` (~2%) in the non-auth suite |
| Freeze animations while capturing (keeps relative timestamps / drift stable) | `expect.toHaveScreenshot.animations` — non-auth config; auth config omits it | `'disabled'` in the non-auth suite |
| Capture viewport size | `use.viewport` in both configs | `1280 x 900` |
| Colour scheme for deterministic rendering | `use.colorScheme` in both configs | `dark` |
| Motion reduction for deterministic rendering | `use.reducedMotion` in both configs | `reduce` |

Deliberately NOT configured:
- Retries and parallelism are disabled (`retries: 0`, `workers: 1`, fullyParallel false) so runs are deterministic but single-threaded.
- Timeouts differ per suite: test timeout is 30 s (non-auth) vs 40 s (auth); each webServer startup timeout is 60 s.
- The auth suite adds `trace: 'retain-on-failure'`.

## Edge cases and limits

What this check deliberately does NOT do, and how it fails:

- **Currently disabled.** The job is gated off (`if: false`) so no PR gets a visual gate today; UI breakage passes CI unnoticed until re-enabled.
- **Informational failure only.** If enabled, failure comments post on failures only (`if: failure()`) and point at artifacts; nothing blocks merge via required status because branch protection wiring is not part of this ticket.
- **Unparseable report fallback.** When failing but `.playwright-report.json` is missing or unparsable, the script posts a generic "could not be parsed" / "no JSON report" message instead of per-spec names.
- **Baseline drift.** Frozen fixture state still ages (relative timestamps shift pixels), which motivates the tolerance but can also surface false positives that need baseline regeneration.
- **Debug build dependency.** The webServer requires a prebuilt debug binary or server startup fails; CI must build before any screenshot can run.

## Code map

Files implementing this feature:

- `.github/workflows/visual-qa.yml` — the whole workflow: trigger, permission block, steps to build/run/post-comment/upload artifacts; currently disabled by its job condition guard.
- `.github/workflows/ci.yml`, `.github/workflows/desktop.yml` — sibling workflows also scaffolded but similarly disabled (`if: false`) on every job as of this writing; they are not part of CXA-F005 but show the same disable pattern across the repo's Actions setup.
- `e2e/playwright.config.ts` — non-auth visual suite definition: port 4517, tolerance + animations settings, dark/reduced-motion viewport profile.
- `e2e/playwright.auth.config.ts` — auth/RBAC suite definition on port 4518 with trace-on-failure.
- e2e specs dirs — each visual spec drives one page then asserts golden screenshots:
  - covered views include chat (`chat.spec.ts`, seeded message search), docs/wiki (`docs.spec.ts`, seeded Deploy-health page render), hybrid mode (`hybrid.spec.ts`) plus agent-log-stream/cost/settings-config/sprint/tickets/views suites listed per snapshot mapping;
    golden baselines live under each `<spec>.ts-snapshots/<name>-darwin.png`.
    (The committed baseline files use a darwin suffix because they were generated on macOS.)
- `e2e/run-server.sh` — boots the local debug binary against a wiped throwaway copy of fixture state on port 4517, unsets ambient DSN/admin env vars for hermetism.
- `e2e/run-server-auth.sh` — same bootstrap but with RBAC enabled and its own isolated state on port 4518; sets COXAGENT_ADMIN_USER/PASSWORD.
- `web/app.css`, `web/js/*.js` — the classic UI this check protects (single shared scope; any UI change must pass the e2e gate per AGENTS.md).

## Related

- Ticket **CXA-F002** — parent ticket whose acceptance criteria CXA-F005 fulfils (auto-run screenshots on PRs + report diffs).
- Wiki page `.wiki/product/CXA-B038.md` — only other page currently in the Product space; documents deploy-secrets handling and shares the same Product-area home.
- AGENTS.md "Running the app you are building" / hexagonal_gate.rs notes — explains why e2e servers must avoid hub port 4000 and how IO discipline applies to application code generally.

