FOLDER: Testing

# Visual QA Merge Gate + Auto-Ticketing

**Keywords:** Visual QA, merge gate, screenshot regression, golden baseline, Playwright toHaveScreenshot, branch protection status check, auto-file Bug ticket, visual regression CI, GitHub Actions visual-qa.yml, CXA-F005 CXA-F006 CXA-B063

## Overview

Visual QA is the automated screenshot-regression gate that blocks Pull Requests targeting `main` when committed UI baselines drift beyond a 2% pixel tolerance. It lives in two places with two different "auto-ticket" meanings that must not be conflated:

1. **CI merge gate** — `.github/workflows/visual-qa.yml` builds `coxagent`, runs the Playwright screenshot suite against committed goldens on port 4517 (hardcoded in `e2e/playwright.config.ts`; served by `run-server.sh`, never dogfood's port 4000), and posts a failure summary comment on the PR thread.
2. **Application-level auto-ticketing** — after shipping a UI ticket to a running deploy (`deploy.host_port` set), `RunCycleUseCase::visual_qa()` has the PD role inspect an actual screenshot and file at most 2 concrete `UI:` Bug tickets.

The CI *auto-file-GitHub-Bug* half of CXA-F006 (a step that ran `gh issue create`) was written in commit `a2fce61` but was **never merged into main** — only the F005/B063 comment-on-failure workflow landed. Anyone reading this page should treat GitHub-side automatic bug filing as planned but absent today.

## How it works

### CI merge gate (shipped)

`.github/workflows/visual-qa.yml` defines exactly one job (`visual-regression`) so GitHub paints one unambiguous green-or-red "Visual regression (screenshots)" check per PR. Order of steps:

1. Checkout → Rust stable toolchain → rust cache.
2. `cargo build --bin coxagent` (debug binary used by the e2e webServer).
3. Install headless Chromium for Playwright (`npx playwright install chromium --with-deps`, working dir `e2e`).
4. Run the suite: `npx playwright test --update-snapshots=off --reporter=json > .playwright-report.json`. The off switch forces comparison against committed goldens rather than regenerating them.
5. Upload artifacts on every outcome (`if: always()`): `e2e/test-results/` and `.playwright-report.json`.
6. On failure only (`if: failure()`): an inline `actions/github-script@v7` reads `.playwright-report.json`, counts failing specs whose result status is `failed`, and calls `issues.createComment` posting a "Visual QA failed" summary with spec names and a link to run artifacts.

The acceptance gate tests live in [`crates/app/tests/visual_qa_gate.rs`](crates/app/tests/visual_qa_gate.rs). They parse the YAML workflow into serde_yaml::Value and assert each contract: exists & valid YAML; triggers on PR opened/synchronize; Ubuntu + headless Chromium + compare-not-regen; passing PRs get a green check and *no* comment (any thread-posting action must be gated on failure); failing PRs post a comment carrying both a failure count and an artifact/diff pointer.

### Application-level Visual QA auto-ticketing (shipped)

After TEST ships a UI feature to a live deploy, the cycle runner calls [`RunCycleUseCase::visual_qa(ticket, report)`](crates/application/src/use_cases/cycle/qa_evidence.rs) which:

1. Returns early unless there is both a headless browser port (`self.shot`) and a deploy host port (`self.config.deploy.host_port`) AND the ticket is flagged UI.
2. Captures `/` of the deployed app through ShotPort capture, writes it via FilesPort to `<work_dir>/.coxagent/ui-shot.png`.
3. Issues an agent request with role PD (`system_prompt(PD)` from prompts.rs) whose task prompt tells PD to LOOK at that PNG with its file tools against the project's design system and return JSON — at most 2 concrete defects.
4. Parses the returned JSON array; for each defect calls [`AddTicketUseCase::execute(...)`](crates/application/src/use_cases/add_ticket.rs) to file a Medium/Small Bug titled ``UI: <title>`` with description noting it came from PD visual QA after `<ticket>` plus `.coxagent/ui-shot.png`, then pushes each new id onto [`report.bugs_filed`](crates/application/src/use_cases/cycle/mod.rs).

Every step is best-effort — no browser, no port, or an unparseable review just skips silently.

## Usage

### See / reproduce the CI gate locally

```bash
# Full local run against committed goldens (port 4517 via run-server.sh):
cd e2e && npx playwright test

# Exactly what CI does — compare against goldens, JSON report:
npx playwright test --update-snapshots=off --reporter=json > .playwright-report.json

# Refresh committed baselines after an INTENTIONAL design change:
npx playwright test --update-snapshots      # or npm run baseline
```

Expected success ends green with no PR comment; expected failure lists which specs exceeded tolerance:

```
1) [chromium] › dashboard.spec.ts › renders correctly
   Error: Screenshot comparison failed ...
```

### Trigger application-level Visual QA auto-ticketing

Three conditions must hold before it fires during cycle execution:

```
deploy.host_port        set in coxagent config   # something real deployed
headless browser shot    configured               # ShotPort present
ticket.has_ui == true                            # UI feature being shipped
```

When those hold, shipping that ticket ends with PD running visual QA; any defects come back as new Open Bug tickets titled ``UI: ...`` filed by author PD via AddTicketUseCase.

## Interface

### `.github/workflows/visual-qa.yml`

| Item | Value |
|------|-------|
| workflow name | `Visual QA` |
| triggers | pull_request types [opened, synchronize] |
| permissions | contents read · issues write · pull-requests write |
| single job | `` visual-regression `` named "Visual regression (screenshots)" |
| runner / budget | ubuntu-latest · timeout-minutes 30 |

Failure-reporter contract enforced by tests:
- parses `.playwright-report.json`;
- counts specs where any result status === 'failed';
- posts via GitHub REST `` issues.createComment `` with count + spec names + artifact link;
- only runs under `` if: failure() `` so green runs stay silent.

### Acceptance-gate tests (`crates/app/tests/visual_qa_gate.rs`)

Behavioral mapping you can depend on:

```
status-check name surfaced : "Visual regression (screenshots)"
workflow path               : .github/workflows/visual-qa.yml
must compare not regen      : "--update-snapshots=off"
single-job rule             : jobs map length == 1
artifact upload            : if(always()) -> e2e/test-results/
comment rule                : github-script/create-comment steps gated on failure()
```

Tests are cargo integration tests reading repo content directly:

```bash
cargo test -p coxagent-app --test visual_qa_gate
```

### Application-level function signature

```rust
pub(super) async fn RunCycleUseCase<S,E>::visual_qa(
    &self,
    ticket: &TicketId,
    report: &mut CycleReport)                 // qa_evidence.rs:200
```

Dependencies resolved inside runtime fields already constructed by wiring in crates/app/src/builders.rs/: ShotPort capture helper + FilesPort write_bytes + engine AgentEnginePort.run + AddTicketUseCase over StateStorePort.

## Configuration

No runtime key turns these features on or off individually beyond deployment wiring:

| Setting / artifact | Type / location | Default | Effect |
|--------------------|------------------|---------|--------|
| golden tolerance | `expect.toHaveScreenshot.maxDiffPixelRatio` in `e2e/playwright.config.ts` | `0.02` (2%) | maximum fraction of differing pixels before a screenshot is a regression |
| worker / parallelism | `workers: 1`, `fullyParallel: false` in playwright.config.ts | serial | keeps runs deterministic across frozen-state screenshots |
| baseline refresh | e2e npm script `baseline` = `playwright test --update-snapshots` | off by default in CI (uses `--update-snapshots=off`) | regenerate committed goldens only on explicit intent |
| deploy host port gate | `deploy.host_port` in coxagent.json + presence of a ShotPort browser + ticket UI flag (`has_ui`) | absent → visual_qa() returns early | application-level PD visual QA only runs against a live deployment of a UI feature |

## Edge cases and limits

What this deliberately does NOT do:

- **No GitHub-side auto bug filing (CXA-F006 CI half).** The committed workflow only *comments* on failing PRs; it does not create issues. The planned ``gh issue create`` step, plus the ``/visual-qa-update`` command and ``visual-qa:update-baseline`` label, exist only in unmerged commit `a2fce61`. Do not document or rely on them as shipped.
- **No branch-protection enforcement here.** The workflow emits the status check that branch protection can require (CXA-B063's requirement), but whether the rule is actually enabled is configured in GitHub's repository settings, not by any file in this repo.
- **No app rebuild / no baseline self-healing.** A drift is never auto-accepted; updating goldens requires an explicit local re-run with snapshots enabled.
- CI jobs build a debug binary (`cargo build --bin coxagent`) — release-only regressions would not surface here.

How it fails today:

- **Unparseable or absent JSON report.** If `.playwright-report.json` is missing or malformed, the github-script posts the generic "could not be parsed / produced no JSON report" message rather than spec names — still red, still a comment, but without diff specifics.
- **Application-level visual_qa is best-effort and silent.** No browser (`self.shot` None), no host port, non-UI ticket, engine run failure, unparseable JSON reply — each just returns early with no ticket filed and no log noise. So absence of new UI bugs proves nothing about coverage; it may simply have skipped.
- **Only one job means one green-or-red signal.** Any step failure (build error, missing Chromium) turns the whole check red even if screenshots are fine — there is no separate "infra broken" vs "pixels drifted" distinction.

## Code map

- `.github/workflows/visual-qa.yml` — the shipped CI merge gate: single job building coxagent + running Playwright against committed goldens on port 4517, uploading artifacts always, posting a failure summary comment via github-script@v7 only on failure.
- [`crates/app/tests/visual_qa_gate.rs`](crates/app/tests/visual_qa_gate.rs) — acceptance gate tests parsing the YAML to enforce every Visual QA contract for CXA-B063 / CXA-F005.
- [`crates/app/src/builders.rs`](crates/app/src/builders.rs) — wiring that constructs runtime fields consumed by application Visual QA: screenshot capture helper (`shot`), FilesPort (`files`) for writing ui-shot.png, storage for evidence uploads.
- [`crates/application/src/use_cases/cycle/qa_evidence.rs`](crates/application/src/use_cases/cycle/qa_evidence.rs) — the two auto-ticketing functions: `collect_evidence()` / its UI path posts DoD screenshots; [`RunCycleUseCase::visual_qa()` at line 200](crates/application/src/use_cases/cycle/mod.rs) files up to two PD-review defects as Open Bug tickets titled ``UI: ...`` into `report.bugs_filed`.

## Related

- CXA-F005 "GitHub Actions visual regression: auto-run screenshots on PRs and report diffs (from CXA-F002)" — landed `.github/workflows/visual-qa.yml` in commit `6e25eaf`; this page documents that merged state.
- CXA-B063 "Branch-protection Visual QA required check impossible" — shipped commit `a551d69` alongside F005; the workflow must exist so the status check can pass. Read both for why one job / one check matters.
- CXA-F006 "Merge gate + auto-ticketing: block PRs with regressions, auto-file Visual QA bugs (from CXA-F002)" — intended to add CI-side auto-filing + baseline-update commands; written in commit `a2fce61` but **not merged into main**, so those pieces are absent here. Check whether it has since landed before relying on any GitHub-side auto-filing.
- AGENTS.md note: any UI change must pass `cd e2e && npx playwright test`.
