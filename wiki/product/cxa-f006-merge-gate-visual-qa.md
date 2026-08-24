FOLDER: Visual QA
# Merge Gate + Auto-Ticketing (CXA-F006)

**Keywords:** merge gate, visual QA, golden screenshot, screenshot diff tolerance, Playwright toHaveScreenshot, CI status check, branch protection required check, DoD evidence, PD visual review, auto-file UI bug

## Overview

CXA-F006 makes pixel-level visual regression a hard merge gate on protected `main` and adds automatic Definition-of-Done (DoD) evidence plus post-deploy Visual QA bug filing that runs inside each agent cycle. It serves developers who change web rendering code - a PR whose screenshots drift gets blocked before merge - and anyone operating CI or repository branch-protection settings. It builds on CXA-F002/F004/F005 intent; CXA-F005 landed the `.github/workflows/visual-qa.yml` workflow itself.

## How it works

Two independent mechanisms share one config knob (screenshot tolerance) and are enforced by one acceptance gate (`crates/app/tests/visual_qa_gate.rs`).

**CI merge gate.** `.github/workflows/visual-qa.yml` defines exactly one job (`visual-regression`, display name **Visual regression (screenshots)**), triggered by pull_request types `[opened, synchronize]`. Steps in order:

1. checkout then rust-toolchain@stable then rust-cache.
2. Build debug binary: `cargo build --bin coxagent`.
3. Install headless Chromium: `npx playwright install chromium --with-deps`, workdir `e2e`.
4. Run the suite with regeneration disabled:
   ```sh
   npx playwright test --update-snapshots=off --reporter=json > .playwright-report.json
   ```
   A bash wrapper propagates Playwright's exit code so any spec over tolerance turns the whole job red; goldens are never regenerated because of `--update-snapshots=off`.
5. Upload artifacts under `if: always()` from e2e/test-results/ plus e2e/.playwright-report.json.
6. Under `if: failure()`, an inline actions/github-script@v7 reads `.playwright-report.json`, counts results whose status is `failed`, renders "N test(s) exceeded the 2% screenshot tolerance" plus each failing spec title with an artifact pointer via github.rest.issues.createComment.

The single green-or-red job is what GitHub branch protection can require as a status check; enforcement lives in repository settings outside this repo.

**Application-level auto-ticketing.** Inside run_cycle (`crates/application/src/use_cases/cycle/mod.rs` ~lines 1059-1085), after TEST ships:

1. On deploy success it calls collect_evidence for both report.feature_done and report.bug_fixed.
2. For a UI feature ticket it calls visual_qa(&id): screenshots the deployed app to `<work_dir>/.coxagent/ui-shot.png` through FilesPort.write_bytes, dispatches PD with system_prompt(PD) asking for at most 2 concrete visible defects as JSON, parses only the last bracketed slice leniently (`raw.find('[')` ..= `raw.rfind(']')`), then files each defect as a Medium/Small Bug titled "UI: ..." onto report.bugs_filed via AddTicketUseCase::execute.
3. collect_missing_evidence retries at most 2 Done/Fixed/Documented tickets per cycle that still lack evidence.
4. attach_test_case_screenshots attaches a real image to pass/fail UI test cases that lack one.
5. file_test_failure files a deduped High Bug titled "Tests failing: ..." when TEST's DoD gate goes red (~line 1126).

Evidence selection depends on Ticket::has_ui(): UI tickets get collect_ui_evidence (screenshot stored via StateStore.add_evidence kind "screenshot", plus an attached TEST comment); non-UI tickets get collect_api_evidence (probes /api/health then `/`, first answer stored as GET url + HTTP status + body snippet kind "api"). When nothing can be captured the record is kind "waived".

## Usage

The merge gate is consumed entirely through GitHub PR automation. Reproduce locally:

```bash
cargo build --bin coxagent                  # required first; run-server.sh refuses if missing
cd e2e && npx playwright install chromium  # first time only
cd e2e && npx playwright test               # compare against committed goldens
cd e2e && npx playwright test --update-snapshots  # refresh baselines AFTER intentional change
```

Playwright boots its own ephemeral server on port **4517** against frozen fixture state (`webServer.command = sh ./run-server.sh ${PORT}`); AGENTS.md forbids binding hub port 4000 during local runs.

GitHub-side flow:
1. Open or push to a PR targeting main.
2. Watch the checks row for **Visual regression (screenshots)**.
3. Green: goldens matched within tolerance; nothing further happens.
4. Red: open artifact visual-qa-results-${{ github.sha }}, read diff images/screenshots from e2e/test-results/, fix rendering or deliberately regenerate goldens locally and commit them.

Failure comment body:

```text
## Visual QA failed

N test(s) exceeded the 2% screenshot tolerance.

Failing spec(s):
- <spec title>

Diff images and screenshots are in the run artifacts:
Visual QA workflow run #<runNumber>.
```

Application-level auto-ticketing fires during cycle execution only when all three hold together:

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
| permissions | contents read, issues write, pull-requests write |
| single job id / display name | visual-regression / Visual regression (screenshots) |
| runner / budget | ubuntu-latest, timeout-minutes 30 |

Steps in order: actions/checkout@v4; dtolnay/rust-toolchain@stable; Swatinem/rust-cache@v2; build debug binary (`cargo build --bin coxagent`); install Chromium (`npx playwright install chromium --with-deps`); run suite (`npx playwright test --update-snapshots=off --reporter=json`, exit code propagated through bash wrapper); upload-artifact@v4 under if() always() paths e2e/test-results/, e2e/.playwright-report.json name visual-qa-results-${{ github.sha }}; github-script@v7 under if() failure() posting summary comment via issues.createComment.

**Acceptance-gate tests** crates/app/tests/visual_qa_gate.rs public test fns:

- workflow_exists_and_is_valid_yaml()
- triggers_on_pull_request_opened_and_synchronize()
- runs_headless_chromium_on_ubuntu()
- passing_prs_get_a_green_check_and_no_comment()
- failing_prs_post_a_comment_with_count_and_diff_link()

YAML helpers parse into serde_yaml::Value preserving reserved top-level keys: load(), single_job(), steps(), command(), condition().

**In-agent methods on RunCycleUseCase<S,E>**, implemented in qa_evidence.rs and orchestrated in mod.rs (~lines 1059-1085):

```rust
pub(super) async fn file_test_failure(&self, summary: &str) -> Option<TicketId>;
pub(super) async fn collect_missing_evidence(&self);
pub(super) async fn collect_api_evidence(&self, ticket: &TicketId, port: u16);
pub(super) async fn collect_ui_evidence(&self, ticket: &TicketId, port: u16);
pub(super) async fn visual_qa(&self, ticket: &TicketId, report: &mut CycleReport);
pub(super) async fn attach_test_case_screenshots(&self);
```

Note some method signatures differ from earlier sibling pages; treat qa_evidence.rs as authoritative before editing code directly.

Evidence lands via StateStore.add_evidence(key = ticket.to_string(), kind ("screenshot" or "api" or "waived"), label, detail). Bugs are filed through AddTicketUseCase.execute(AddTicketInput).

## Configuration

The merge gate's screenshot comparison knobs live in `e2e/playwright.config.ts`:

| Setting | Default |
|---------|---------|
| expect.toHaveScreenshot.maxDiffPixelRatio | 0.02 (the "2%" referenced in failure comments) |
| expect.toHaveScreenshot animations | disabled |
| use.viewport / colorScheme / reducedMotion | 1280x900 / dark / reduce |
| fullyParallel / workers / retries / timeout | false / 1 / 0 / 30_000 |
| webServer.command (+ port constant PORT=4517) | sh ./run-server.sh ${PORT} |

CI behavior is configured inside `.github/workflows/visual-qa.yml` itself: trigger types [opened, synchronize]; runner ubuntu-latest; timeout-minutes 30; env CARGO_TERM_COLOR=always; artifact name template visual-qa-results-${{ github.sha }}.

For the application side there is no runtime key that turns it off individually beyond deployment wiring. The effective gate is Config.deploy.host_port being unset: then collect evidence and visual QA return early (gate off). Which evidence kind fires depends on Ticket::has_ui(). Whether branch protection actually requires **Visual regression (screenshots)** as a status check lives in GitHub repository settings outside this repo.

## Edge cases and limits

What this deliberately does NOT do:

- **No GitHub-side auto bug filing.** The committed workflow `.github/workflows/visual-qa.yml` only comments on failing PRs; it does not create issues and never regenerates baselines automatically. Blocking merges is enforced externally by branch protection requiring its status check.
- **Jobs are currently disabled with `if: false`.** As of HEAD every CI job in `.github/workflows/visual-qa.yml`, `ci.yml`, and `desktop.yml` carries an `if: false` guard, so the Visual QA check does NOT actually run on PRs today. The workflow is structurally correct (and the acceptance gate validates it), but re-enabling it means removing that guard. Do not assume a green signal exists until it is un-disabled.
- **No branch-protection enforcement inside this repo.** The workflow emits the status check that branch protection can require, but enabling the rule lives in GitHub repository settings.
- **Single-job constraint.** The acceptance gate asserts exactly one job exists; adding a second job breaks passing_prs_get_a_green_check_and_no_comment's single green-or-red assumption.
- **Goldens never regenerate in CI.** `--update-snapshots=off` means an intended redesign fails the check until baselines are deliberately refreshed locally and committed.
- **Debug build only.** CI builds `cargo build --bin coxagent`, so release-only regressions would not surface here.

How it fails today:

- **Unparseable or absent JSON report.** If `.playwright-report.json` is missing or malformed, github-script posts the generic "could not be parsed / produced no JSON report" message instead of spec names - still red and commented, but without diff specifics.
- **Application-level evidence and visual QA are best-effort and waived, never fatal.** Missing browser (`self.shot` None), storage, probe, no host port, non-UI ticket, engine run failure, or an unparseable JSON review each just records Evidence kind "waived" or skips silently; none block TEST. So absence of new UI bugs proves nothing about coverage - it may simply have skipped.
- **One green-or-red signal with no infra/pixel distinction.** Any step failure (build error, missing Chromium install) turns the whole check red even when screenshots are fine.

## Code map

Files that implement CXA-F006:

- `.github/workflows/visual-qa.yml` - the shipped Visual QA merge-gate workflow: single visual-regression job building coxagent + running Playwright against committed goldens (`--update-snapshots=off`), uploading artifacts always(), posting a failure summary comment via github-script@v7 only on failure(). Currently disabled via `if: false`.
- `crates/app/tests/visual_qa_gate.rs` - acceptance-gate tests parsing the YAML into serde_yaml::Value to enforce every Visual QA contract (five public test fns plus helpers load(), single_job(), steps(), command(), condition()).
- `crates/application/src/use_cases/cycle/mod.rs` - run_cycle orchestration calling collect_evidence / visual_qa / collect_missing_evidence / file_test_failure / attach_test_case_screenshots after deploy (~lines 1059-1085).
- `crates/application/src/use_cases/cycle/qa_evidence.rs` - in-agent DoD evidence + visual-QA methods: file_test_failure, collect_missing_evidence/_ui/_api (dispatched by collect_evidence), visual_qa, attach_test_case_screenshots.
- `crates/app/src/builders.rs` - wiring that constructs runtime fields consumed by evidence: shot (ChromeScreenshot), files (FilesPort.write_bytes for ui-shot.png), storage (LocalStorage or S3/MinIO blob storage), probe (HttpProbe).
- `e2e/playwright.config.ts` - golden tolerance maxDiffPixelRatio 0.02, viewport/reduced-motion presets, webServer boot on port 4517 via run-server.sh.
- e2e/specs/*.spec.ts (+ snapshot dirs) - Playwright specs whose committed goldens back Visual QA.

## Related

Related tickets and pages:

- CXA-F005 "GitHub Actions visual regression: auto-run screenshots on PRs and report diffs" (from CXA-F002) landed `.github/workflows/visual-qa.yml`.
- CXA-B063 "Branch-protection Visual QA required check impossible" / CXA-B070 job display-name fix - enabling branch protection itself is a GitHub repository setting outside this repo.
- Sibling page cxa-f004-playwright-golden-suite.md covers which views/specs produce the goldens this gate compares against; cxa-f022-bug-burndown-prompt-system.md also consumes regression evidence before closure.
