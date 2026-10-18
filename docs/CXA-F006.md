FOLDER: Engineering

# Visual QA Merge Gate + DoD Evidence

**Keywords:** Visual QA merge gate, required status check, branch protection main, golden screenshots, visual regression CI, Playwright toHaveScreenshot, maxDiffPixelRatio 0.02, update-snapshots=off, visual_qa_gate.rs, qa_evidence.rs

## Overview

CXA-F006 makes pixel-level visual regression a hard merge gate on protected `main` and gives every shipped ticket Definition-of-Done (DoD) evidence. The GitHub Actions workflow `.github/workflows/visual-qa.yml` runs Playwright against committed golden screenshots on every pull request targeting main; GitHub branch protection requires that single status check so its red/green outcome blocks merges outright. In parallel, `RunCycleUseCase` collects a real screenshot or live API proof after each deploy and runs a post-deploy visual-QA pass that files concrete UI bugs. It serves developers who change rendering code (the failing-PR comment names exactly which specs drifted) and anyone operating CI or branch-protection settings.

## How it works

The feature is two independent mechanisms tied together by one shared config knob (screenshot tolerance) and covered by one acceptance gate.

**CI merge gate.** `.github/workflows/visual-qa.yml` defines exactly one job `visual-regression` (display name **Visual regression (screenshots)**), triggered by `on.pull_request.types: [opened, synchronize]`. Steps in order:

1. Checkout → Rust stable toolchain → rust cache.
2. Build debug binary: `cargo build --bin coxagent`.
3. Install headless Chromium: `npx playwright install chromium --with-deps`, workdir `e2e`.
4. Run the suite with snapshots off:
   ```sh
   npx playwright test --update-snapshots=off --reporter=json > .playwright-report.json
   ```
   The exit code is propagated through a bash wrapper (`set +e ... code=$?; exit "$code"`) so any spec over tolerance turns the whole job red; goldens are never regenerated because of `--update-snapshots=off`.
5. Upload artifacts under `if: always()` from e2e/test-results/ + e2e/.playwright-report.json.
6. Under `if: failure()`, an inline actions/github-script@v7 reads `.playwright-report.json`, counts tests whose result status is `failed`, renders "N test(s) exceeded the 2% screenshot tolerance" plus each failing spec title with an artifact pointer via `github.rest.issues.createComment`.

Because HEAD names this single job **Visual regression (screenshots)** (YAML line 15), it satisfies GitHub branch protection's required-status-check of that exact name.

**In-agent DoD evidence.** Inside run_cycle in crates/application/src/use_cases/cycle/mod.rs (call sites ~lines 1050-1153), after TEST ships:

1. collect_evidence(ticket): returns early when Config.deploy.host_port is unset ("gate off"); otherwise dispatches on Ticket::has_ui().
   - collect_ui_evidence(ticket): ShotPort shot.capture("http://127.0.0.1:{port}/") captures `/`; storage.put stores it under proj/{pid}/evidence-{key}.png; StateStore add_evidence records kind "screenshot" with URL+size; posts an attached TEST comment. No browser/storage/render records kind "waived".
   - collect_api_evidence(ticket): probes /api/health then / in order via self.probe.get(); first answer stored as GET url + HTTP status + body snippet ("api" proof); no probe/no answer records "waived".
2. collect_missing_evidence(): retries at most 2 Done/Fixed/Documented tickets per cycle still missing any evidence.
3. visual_qa(ticket, &mut report): only fires when this cycle shipped a UI feature AND host+shot present:
   - writes `<work_dir>/.coxagent/ui-shot.png` through FilesPort write_bytes;
   - dispatches PD with system_prompt(PD) asking for at most 2 concrete visible defects as JSON;
   - parses only the last bracketed slice leniently;
   - files each defect as Medium/Small Bug titled "UI: ..." into report.bugs_filed.
4. attach_test_case_screenshots(): attaches a screenshot onto already-pass/fail UI test cases that lack one.
5. file_test_failure(summary): files a deduped High Bug titled "Tests failing: ..." when TEST's DoD gate goes red.

## Usage

The merge gate is consumed entirely through GitHub PR automation — there is no CLI or runtime service to invoke it.

```bash
cd e2e && npx playwright test                        # compare against committed goldens
cd e2e && npx playwright test --update-snapshots     # refresh baselines AFTER intentional change
```

Local runs need the debug binary (`cargo build --bin coxagent`); run-server.sh boots an ephemeral server on port 4517 against frozen fixture state so screenshots are deterministic — AGENTS.md forbids binding hub port 4000 during local runs.

GitHub-side flow:
1. Open or push to a PR.
2. Watch the checks row for **Visual regression (screenshots)**.
3. Green — goldens matched within tolerance; nothing further happens.
4. Red — open artifact visual-qa-results-${{ github.sha }}, read diff images/screenshots from e2e/test-results/, fix rendering or deliberately regenerate goldens locally and commit them.

The failure comment body:

```text
## Visual QA failed

N test(s) exceeded the 2% screenshot tolerance.

Failing spec(s):
- <spec title>

Diff images and screenshots are in the run artifacts:
Visual QA workflow run #<runNumber>.
```

Application-level Visual QA auto-ticketing fires during cycle execution only when all three hold:

```
deploy.host_port    set in coxagent config   # something real deployed
headless browser shot configured             # ShotPort present
ticket.has_ui == true                        # UI feature being shipped
```

Shipping such a ticket ends with PD filing up to two Open Bug tickets titled ``UI: ...`` via AddTicketUseCase::execute(AddTicketInput { priority: Medium, complexity: Small }) pushed onto report.bugs_filed.

Acceptance-gate tests run as cargo integration tests:

```bash
cargo test -p coxagent-app --test visual_qa_gate
```

## Interface

**.github/workflows/visual-qa.yml**

| Field | Value |
|-------|-------|
| workflow name | Visual QA |
| triggers | pull_request types [opened, synchronize] |
| permissions | contents read · issues write · pull-requests write |
| single job id / display name | visual-regression / **Visual regression (screenshots)** |
| runner / budget | ubuntu-latest · timeout-minutes 30 |

Steps in order: actions/checkout@v4 · dtolnay/rust-toolchain@stable · Swatinem/rust-cache@v2 · Build debug binary (`cargo build --bin coxagent`) · Install Chromium (`npx playwright install chromium --with-deps`) · Run suite (`npx playwright test --update-snapshots=off --reporter=json`, exit code propagated through bash wrapper capturing $?) · upload-artifact@v4 under if(): always() paths e2e/test-results/, e2e/.playwright-report.json name visual-qa-results-${{ github.sha }} · github-script@v7 under if(): failure() posting summary comment via issues.createComment.

**Acceptance-gate tests** crates/app/tests/visual_qa_gate.rs exposes these public test fns:

- workflow_exists_and_is_valid_yaml()
- triggers_on_pull_request_opened_and_synchronize()
- runs_headless_chromium_on_ubuntu()
- passing_prs_get_a_green_check_and_no_comment()
- failing_prs_post_a_comment_with_count_and_diff_link()

Helpers parse `.github/workflows/visual-qa.yml` into serde_yaml::Value preserving reserved top-level keys: load(), single_job(), steps(), command(), condition(). The surfaced status-check display name is *Visual regression (screenshots)* per the jobs map.

**In-agent methods on RunCycleUseCase<S,E>** (crates/application/src/use_cases/cycle/mod.rs orchestrates them; crates/application/src/use_cases/cycle/qa_evidence.rs defines them):

```rust
pub(super) async fn file_test_failure(&self, summary: &str) -> Option<TicketId>;
pub(super) async fn collect_evidence(&self, ticket: &TicketId);
pub(super) async fn collect_missing_evidence(&self);
pub(super) async fn collect_api_evidence(&self, ticket: &TicketId, port: u16);
pub(super) async fn collect_ui_evidence(&self, ticket: &TicketId, port: u16);
pub(super) async fn visual_qa(&self, ticket: &TicketId, report: &mut CycleReport);
pub(super) async fn attach_test_case_screenshots(&self);
```

Evidence lands via StateStore `add_evidence(ticket_key /*ticket.to_string()*/, kind /*"screenshot" | "api" | "waived"/ , label, detail)` into ProjectState.ticket_evidence. Bugs are filed through AddTicketUseCase::new(Arc::clone(&store)).execute(AddTicketInput{...}).

## Configuration

The merge gate's screenshot comparison knobs live in e2e/playwright.config.ts:

| Setting | Default |
|---------|---------|
| expect.toHaveScreenshot.maxDiffPixelRatio | 0.02 (the 2% referenced in failure comments) |
| expect.toHaveScreenshot.animations | disabled |
| use.viewport / colorScheme / reducedMotion | 1280x900 / dark / reduce |
| fullyParallel / workers / retries / timeout | false / 1 / 0 / 30_000 |
| webServer.command | run-server.sh on port 4517 against frozen fixture |

CI behavior is configured inside `.github/workflows/visual-qa.yml` itself: trigger types [opened,synchronize]; runner ubuntu-latest; timeout-minutes 30; env CARGO_TERM_COLOR=always; artifact name template visual-qa-results-${{ github.sha }}.

For the application side there is no runtime key that turns it off individually beyond deployment wiring. The effective gate is Config.deploy.host_port being unset — then collect evidence and visual QA return early ("gate off"). Which evidence kind fires depends on Ticket::has_ui(). Whether branch protection actually requires **Visual regression (screenshots)** as a status check lives in GitHub repository settings outside this repo (see Related).

## Edge cases and limits

What this deliberately does NOT do:

- **No GitHub-side auto bug filing.** The committed workflow `.github/workflows/visual-qa.yml` only *comments* on failing PRs; it does not create issues and never regenerates baselines automatically. Blocking merges is enforced externally by branch protection requiring its status check.
   - Note a conflict between the two existing F006 wiki pages: docs/wiki/product/cxa-f006... states CI posts but does not open bugs; docs/wiki/testing/cxa-f006... reports a planned `gh issue create` step written in commit a2fce61 that was never merged into main. The authoritative YAML at HEAD confirms no issue-create step exists today.
- **No branch-protection enforcement inside this repo.** The workflow emits the status check that branch protection can require, but enabling the rule lives in GitHub repository settings.
- **Single-job constraint.** The acceptance gate asserts exactly one job exists; adding a second job breaks passing_prs_get_a_green_check_and_no_comment's single green-or-red assumption.
- **Goldens never regenerate in CI.** `--update-snapshots=off` means an intended redesign fails the check until baselines are deliberately refreshed locally (`npx playwright test --update-snapshots`) and committed.
- **Debug build only.** CI builds `cargo build --bin coxagent`, so release-only regressions would not surface here.

How it fails today:

- **Unparseable or absent JSON report.** If `.playwright-report.json` is missing or malformed, github-script posts the generic "could not be parsed / produced no JSON report" message instead of spec names — still red and commented, but without diff specifics.
- **Application-level evidence + visual QA are best-effort and waived, never fatal.** Missing browser (`self.shot` None), storage, probe, no host port, non-UI ticket, engine run failure, or an unparseable JSON review each just records Evidence kind "waived" or skips silently — none block TEST. So absence of new UI bugs proves nothing about coverage; it may simply have skipped.
- **One green-or-red signal with no infra/pixel distinction.** Any step failure (build error, missing Chromium install) turns the whole check red even when screenshots are fine.

## Code map

- `.github/workflows/visual-qa.yml` — the shipped Visual QA merge-gate workflow: single `visual-regression` job building coxagent + running Playwright against committed goldens on port 4517 (`--update-snapshots=off`), uploading artifacts always(), posting a failure summary comment via github-script@v7 only on failure().
- `crates/app/tests/visual_qa_gate.rs` — acceptance-gate tests parsing `.github/workflows/visual-qa.yml` into serde_yaml::Value to enforce every Visual QA contract (the five public test fns + YAML helpers load(), single_job(), steps(), command(), condition()).
- `crates/application/src/use_cases/cycle/mod.rs` — run_cycle orchestration: calls collect_evidence / visual_qa / collect_missing_evidence / file_test_failure / attach_test_case_screenshots after deploy and after tests (~lines 1050-1153).
- `crates/application/src/use_cases/cycle/qa_evidence.rs` — in-agent DoD evidence + visual-QA methods: file_test_failure, collect_evidence/_ui/_api, collect_missing_evidence, visual_qa, attach_test_case_screenshots.
- `crates/application/src/use_cases/add_ticket.rs` — AddTicketUseCase::execute used by both file_test_failure ("Tests failing: ...") and visual_qa ("UI: ...") to open Bug tickets.
- `crates/app/src/builders.rs` — wiring that constructs runtime fields consumed by evidence: shot (ScreenshotPort capture), files (FilesPort write_bytes for ui-shot.png), storage, probe.
- `e2e/playwright.config.ts` — golden-screenshot tolerance (maxDiffPixelRatio 0.02), viewport/reduced-motion presets, webServer boot on port 4517 via run-server.sh.
- `e2e/specs/*.spec.ts` (+ -snapshots dirs) — Playwright specs whose committed goldens back Visual QA.

## Related

Related tickets and pages:
- CXA-F005 "GitHub Actions visual regression: auto-run screenshots on PRs and report diffs" (from CXA-F002) — landed `.github/workflows/visual-qa.yml`.
- CXA-B063 "Branch-protection Visual QA required check impossible" / CXA-B070 job display-name fix — the workflow must exist so the status check can pass; enabling branch protection itself is a GitHub repository setting.
- The evidence layer also backs the F022 bug burn-down (`crates/app/tests/burndown_f022_gate.rs`) which asserts regression evidence before closure.
- Existing sibling wiki pages for this feature that may disagree with HEAD: docs/wiki/product/cxa-f006-merge-gate-visual-qa.md and docs/wiki/testing/cxa-f006-visual-qa-merge-gate.md.
