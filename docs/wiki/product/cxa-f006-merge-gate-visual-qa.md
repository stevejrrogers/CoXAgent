FOLDER: -
# Merge Gate + Visual QA Auto-Ticketing

**Keywords:** visual QA, merge gate, golden screenshot, screenshot diff tolerance, Playwright, CI status check, branch protection, required status check, DoD evidence

## Overview
CXA-F006 closes two gaps around shipped UI quality. First it provides a CI merge gate that runs a Playwright golden-screenshot suite on every pull request so visual regressions surface before merge as a single green-or-red `Visual QA` status check. Second it gives the agent cycle an in-process pass that collects Definition-of-Done (DoD) evidence after each successful deploy: a real screenshot for UI tickets or a captured request/response for API tickets, plus a post-deploy visual-QA review that files concrete UI bugs automatically.

## How it works
The feature is two independent mechanisms tied together by one shared config knob (screenshot tolerance) and one acceptance-test concern (a golden-screenshot pass).

**Merge gate (GitHub Actions).** The workflow `.github/workflows/visual-qa.yml` runs exactly one job (`visual-regression`, display name **Visual QA**) on every pull_request of types `opened` and `synchronize`. Steps: build the debug binary (`cargo build --bin coxagent`, used by Playwright's webServer), install headless Chromium (`npx playwright install chromium --with-deps`), then run `npx playwright test --update-snapshots=off --reporter=json > .playwright-report.json`. Comparing with snapshots off means goldens are never silently regenerated in CI; any failed spec fails the job because the step propagates Playwright's exit code through a shell wrapper. Artifacts upload unconditionally (`if: always()`) so diffs stay inspectable on every outcome; on failure only (`if: failure()`) an inline actions/github-script step reads `.playwright-report.json`, collects every result whose status is `failed`, renders "N test(s) exceeded the 2% screenshot tolerance", lists failing spec titles plus an artifact pointer, and posts via github.rest.issues.createComment.

Because HEAD carries CXA-B070's fix that names this single job exactly **Visual QA**, the job satisfies GitHub branch protection's required-status-check of that name. All five acceptance criteria are locked as tests in `crates/app/tests/visual_qa_gate.rs`, which parses `.github/workflows/visual-qa.yml` straight into a serde_yaml value tree to preserve YAML's reserved unquoted top-level trigger key.

**In-agent DoD evidence + visual QA.** Inside `RunCycleUseCase::run_cycle_impl` (crates/application/src/use_cases/cycle/mod.rs), after a deploy succeeds:

1. collect_evidence(ticket) returns early when no host port is configured (nothing deployed to prove against); otherwise it dispatches on the ticket's UI flag:
   - collect_ui_evidence(ticket): capture via shot.capture; upload PNG through storage under proj/{pid}/evidence-{key}.png; record Evidence kind "screenshot" with URL+size via state add_evidence; post comment with attachment. When there is no browser or no storage it records kind "waived".
   - collect_api_evidence(ticket): probe /api/health then / in order via probe.get; store first answer as GET url, HTTP code, body snippet as "api" proof comment; no probe or no answer records kind "waived".
2. collect_missing_evidence() retries at most 2 tickets per cycle for Done/Fixed/Documented tickets still missing any evidence.
3. visual_qa(ticket, &mut report) runs only when this cycle shipped a UI feature and host+shot exist: writes .coxagent/ui-shot.png through the files port; dispatches the PD agent asking for at most 2 concrete visible defects in JSON; parses the last bracketed slice leniently; files each found defect as a Medium/Small bug prefixed "UI:" into report.bugs_filed.
4. attach_test_case_screenshots() attaches an image onto already-marked pass/fail test cases that lack one.

## Usage
The merge gate is consumed entirely through GitHub PR automation; there is no CLI or runtime service to invoke. To reproduce locally before pushing:

```bash
cd e2e && npx playwright test          # compare against committed goldens
cd e2e && npx playwright test --update-snapshots   # regenerate baselines (only after review)
```

This requires the debug binary (`cargo build --bin coxagent`); run-server.sh boots an ephemeral server on port 4517 against a frozen state fixture so screenshots are deterministic. AGENTS.md forbids binding hub port 4000 during local runs — the suite already avoids it.

GitHub-side flow:
1. Open or push to a PR.
2. Watch the checks row for **Visual QA**.
3. Green: goldens matched within tolerance, nothing else happens.
4. Red: click through to the run, read the comment listing failing specs and artifact pointer, download diffs/screenshots from artifacts, fix.

The comment body CI posts on failure renders as:

```text
## Visual QA failed

N test(s) exceeded the 2% screenshot tolerance.

Failing spec(s):
- <spec title>

Diff images and screenshots are in the run artifacts:
Visual QA workflow run #<runNumber>.
```

## Interface
**GitHub workflow** `.github/workflows/visual-qa.yml`:

| Field | Value |
|-------|-------|
| Trigger | pull_request types [opened, synchronize] |
| Job id / display name | visual-regression / **Visual QA** |
| Permissions | contents: read, issues: write, pull-requests: write |
| Runner / timeout | ubuntu-latest / 30 min |

Steps in order: actions/checkout@v4; dtolnay/rust-toolchain@stable; Swatinem/rust-cache@v2; build debug binary (cargo build --bin coxagent); install Chromium (npx playwright install chromium --with-deps); run suite (npx playwright test --update-snapshots=off --reporter=json, exit code propagated); upload-artifact@v4 under `if: always()` (paths e2e/test-results/, e2e/.playwright-report.json, name visual-qa-results-${{ github.sha }}); github-script@v7 under `if: failure()` posting the summary comment.

**Acceptance gate tests** `crates/app/tests/visual_qa_gate.rs` exposes these test fns:
- workflow_exists_and_is_valid_yaml()
- triggers_on_pull_request_opened_and_synchronize()
- runs_headless_chromium_on_ubuntu()
- passing_prs_get_a_green_check_and_no_comment()
- failing_prs_post_a_comment_with_count_and_diff_link()

Helpers: load(), single_job(), steps(), command(), condition().

**In-agent evidence methods** on `RunCycleUseCase` (crates/application/src/use_cases/cycle/qa_evidence.rs): collect_evidence, collect_ui_evidence, collect_api_evidence, collect_missing_evidence, visual_qa, attach_test_case_screenshots. Evidence is stored via ProjectState::add_evidence(ticket, kind, label, detail) into the ticket_evidence map.

## Configuration
The merge gate's screenshot comparison knobs live in `e2e/playwright.config.ts`:
- `expect.toHaveScreenshot.maxDiffPixelRatio` — default `0.02` (the 2% tolerance referenced in the failure comment).
- `expect.toHaveScreenshot.animations` — `disabled`.
- `use.viewport` — 1280x900; `use.colorScheme` — dark; `use.reducedMotion` — reduce.
- Suite-wide: fullyParallel false, workers 1, retries 0, timeout 30_000.
- webServer.command runs run-server.sh on port 4517 against a frozen fixture.

The CI workflow behavior is configured inside `.github/workflows/visual-qa.yml`: trigger types, runner, timeout-minutes (30), CARGO_TERM_COLOR=always env, artifact name template visual-qa-results-${{ github.sha }}.

For the in-agent side, the effective gate is project configuration: when Config.deploy.host_port is unset/absent there is nothing deployed to prove against and collect_evidence returns early (gate off). Which evidence kind fires depends on Ticket::has_ui().

## Edge cases and limits
- **Does not auto-file GitHub bug tickets anymore.** The pipeline reports regressions to the PR thread but does not open a Bug; blocking merges is enforced externally by branch protection requiring the check.
- **Single-job constraint.** The acceptance gate asserts exactly one job exists; adding a second job breaks passing_prs_get_a_green_check_and_no_comment's single green-or-red assumption.
- **Goldens never regenerate in CI.** --update-snapshots=off means a genuinely changed UI (intended redesign) fails the check until baselines are deliberately refreshed locally and committed.
- **Comment only on failure, artifacts on every outcome.** A step that can write to the thread must be failure()-gated; uploads are always() so red runs stay inspectable.
- **Evidence capture is best-effort and waived, never fatal.** Missing browser, storage, probe, or an app that does not answer records an Evidence entry of kind "waived" with a reason — it does not block the TEST gate. If Config.deploy.host_port is unset there is no evidence pass at all.
- **visual_qa needs PD cooperation.** It parses only the last bracketed JSON slice; an unparseable review or failed engine run simply skips filing bugs (capped at 2). Screenshot capture writes .coxagent/ui-shot.png through files port; without that port nothing proceeds.

## Code map
- `.github/workflows/visual-qa.yml` — the Visual QA merge-gate workflow: single visual-regression job, build + Chromium install + snapshot-off suite + always() artifacts + failure()-gated comment script.
- `crates/app/tests/visual_qa_gate.rs` — acceptance tests parsing that workflow (the 5 public test fns and YAML helpers).
- `crates/application/src/use_cases/cycle/qa_evidence.rs` — in-agent DoD evidence + visual-QA methods: file_test_failure, collect_evidence/_ui/_api, collect_missing_evidence, visual_qa, attach_test_case_screenshots.
- `crates/application/src/use_cases/cycle/mod.rs` — orchestration: calls collect_evidence / visual_qa / collect_missing_evidence / file_test_failure inside run_cycle after deploy and after tests.
- `crates/application/src/state/mod.rs` — ProjectState.ticket_evidence map and add_evidence(ticket, kind, label, detail) used by both the evidence flow and F022 regression gates.
- `e2e/playwright.config.ts` — golden-screenshot tolerance (maxDiffPixelRatio 0.02), viewport/reduced-motion presets, webServer boot on port 4517 via run-server.sh.
- `e2e/specs/*.spec.ts` (+ -snapshots dirs) — Playwright specs whose committed goldens back Visual QA.

## Related
Related tickets: CXA-F002 / CXA-F004 (original intent — catch UI drift from goldens and gate merges); CXA-B070 (job display-name fix enabling branch-protection required-status-check matching); CXA-F005 / B063 (converged on the same deliverable that landed the workflow). The evidence layer also backs the F022 bug burn-down, whose acceptance gate `crates/app/tests/burndown_f022_gate.rs` asserts regression evidence before closure. The config-drift coverage page lives alongside this one at `docs/wiki/product/CXA-F021-config-drift-coverage-gate.md`. Transport/state-store hand-off notes: `.claude/handoff-rest-runner.md`.
