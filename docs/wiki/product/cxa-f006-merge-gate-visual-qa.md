FOLDER: -

# Merge Gate + Visual QA Auto-Ticketing

**Keywords:** visual QA, merge gate, golden screenshot, screenshot diff tolerance, Playwright toHaveScreenshot, CI status check, branch protection required check, DoD evidence, PD visual review, UI bug auto-filing

## Overview
CXA-F006 ships two gates that surface UI quality before and after merge. First a CI merge gate runs a Playwright golden-screenshot suite on every pull request so visual regressions block a PR as one red **Visual regression (screenshots)** status check instead of reaching main silently. Second the agent cycle collects Definition-of-Done evidence after each successful deploy (a real screenshot for UI tickets or a captured request/response for API tickets) and runs a post-deploy PD visual-QA pass that files concrete UI bugs automatically via `RunCycleUseCase::visual_qa`. It builds on CXA-F002/F004/F005 intent; CXA-F005 landed the workflow itself.

## How it works
Two independent mechanisms share one config knob (screenshot tolerance) and one acceptance-test concern.

**CI merge gate.** `.github/workflows/visual-qa.yml` runs exactly one job (`visual-regression`, display name **Visual regression (screenshots)**) on every PR of types `opened` and `synchronize`. Steps: build the debug binary (`cargo build --bin coxagent`, used by Playwright's webServer), install headless Chromium (`npx playwright install chromium --with-deps`), then run the suite with regeneration disabled (`npx playwright test --update-snapshots=off --reporter=json > .playwright-report.json`) while propagating Playwright's exit code through a shell wrapper so any failed spec fails the job. Artifacts upload unconditionally (`if: always()`) so diffs stay inspectable; on failure only (`if: failure()`) an inline actions/github-script step reads `.playwright-report.json`, collects every result whose status is `failed`, renders "N test(s) exceeded the 2% screenshot tolerance" plus failing spec titles and an artifact pointer, then posts via github.rest.issues.createComment. The workflow does **not** open GitHub issues — blocking merges comes from branch protection requiring this status check; enforcement lives in repository settings, not in any file here.

The five acceptance criteria are locked as tests in `crates/app/tests/visual_qa_gate.rs`, which parses `.github/workflows/visual-qa.yml` into a serde_yaml value tree to preserve YAML's reserved unquoted top-level trigger key.

> **Branch-protection gotcha.** Rules must reference an actual surfaced status-check *name*. That name today is the job display name **"Visual regression (screenshots)"**. CXA-B070's commit renaming it to exactly "Visual QA" exists only on an unmerged branch at HEAD; do not configure branch protection against "Visual QA" until that rename lands or no check will match.

**In-agent DoD evidence + visual QA.** Inside `RunCycleUseCase::run_cycle_impl` (crates/application/src/use_cases/cycle/mod.rs), after a deploy succeeds:

1. collect_evidence(ticket) returns early when no host port is configured (nothing deployed to prove against); otherwise it dispatches on the ticket's UI flag:
   - collect_ui_evidence(ticket): capture via shot.capture; upload PNG through storage under proj/{pid}/evidence-{key}.png; record Evidence kind "screenshot" with URL+size via ProjectState::add_evidence; post comment with attachment. No browser/storage -> kind "waived".
   - collect_api_evidence(ticket): probe `/api/health` then `/` in order via probe.get; store first answer as GET url + HTTP code + body snippet as "api" proof comment; no probe or no answer -> kind "waived".
2. collect_missing_evidence() retries at most 2 tickets per cycle for Done/Fixed/Documented tickets still missing any evidence.
3. visual_qa(ticket, &mut report) runs only when this cycle shipped a UI feature AND host+shot exist: writes `.coxagent/ui-shot.png` through FilesPort; dispatches role PD asking for at most 2 concrete visible defects as JSON against `.coxagent/ui-shot.png` using its file tools vs the design system; parses the last bracketed slice leniently; files each found defect as a Medium/Small Bug titled ``UI: ...`` into report.bugs_filed via AddTicketUseCase.
4. attach_test_case_screenshots() attaches an image onto already-marked pass/fail test cases that lack one.

Separately file_test_failure(summary) files a High Bug deduped under marker ``Tests failing:`` when TEST leaves DoD red.

## Usage
The merge gate is consumed entirely through GitHub PR automation — no CLI or runtime service to invoke locally beyond reproducing:

```bash
cd e2e && npx playwright test          # compare against committed goldens
cd e2e && npx playwright test --update-snapshots   # regenerate baselines ONLY after review
```

This requires the debug binary (`cargo build --bin coxagent`); run-server.sh boots an ephemeral server on port 4517 against a frozen state fixture so screenshots are deterministic. AGENTS.md forbids binding hub port 4000 during local runs — already avoided here.

GitHub-side flow:
1. Open or push to a PR.
2. Watch checks for **Visual regression (screenshots)**.
3. Green: goldens matched within tolerance — nothing else happens.
4. Red: open the run comment listing failing specs + artifact pointer; download diffs/screenshots from artifacts and fix.

The failure comment renders as:

```text
## Visual QA failed

N test(s) exceeded the 2% screenshot tolerance.

Failing spec(s):
- <spec title>

Diff images and screenshots are in the run artifacts:
Visual QA workflow run #<runNumber>.
```

Application-level auto-ticketing fires inside cycle execution only when all three hold simultaneously:

```
deploy.host_port        set in coxagent config   # something real deployed
headless browser shot    configured               # ShotPort present
ticket.has_ui == true                            # UI feature being shipped
```

When they do, shipping that ticket ends with PD running visual QA over `.coxagent/ui-shot.png`, returning new Open Bug tickets titled ``UI: ...`` into report.bugs_filed if visible defects are found.

## Interface

**.github/workflows/visual-qa.yml:**

| Field | Value |
|-------|-------|
| Workflow display name | Visual QA |
| Trigger | pull_request types [opened, synchronize] |
| Permissions | contents read / issues write / pull-requests write |
| Job id / display name | visual-regression / Visual regression (screenshots) |
| Runner / timeout | ubuntu-latest / 30 min |

Steps in order: actions/checkout@v4 -> dtolnay/rust-toolchain@stable -> Swatinem/rust-cache@v2 -> cargo build --bin coxagent -> install Chromium -> playwright test --update-snapshots=off --reporter=json (exit code propagated) -> upload-artifact@v4 under `if: always()` -> github-script@v7 under `if: failure()` posting summary comment.

Failure-reporter contract enforced by tests:
- parses `.playwright-report.json`;
- counts specs where any result status === 'failed';
- posts via GitHub REST issues.createComment with count + spec names + artifact link;
- only under `if: failure()` so green runs stay silent;
- unparseable/missing report falls back to generic message but stays red + commented.

**Acceptance-gate tests** crates/app/tests/visual_qa_gate.rs exposes these public test fns:
workflow_exists_and_is_valid_yaml(), triggers_on_pull_request_opened_and_synchronize(), runs_headless_chromium_on_ubuntu(), passing_prs_get_a_green_check_and_no_comment(), failing_prs_post_a_comment_with_count_and_diff_link(). Helpers load(), single_job(), steps(), command(), condition() live alongside them.
Run with:

```bash
cargo test -p coxagent-app --test visual_qa_gate
```

**In-agent methods** on `RunCycleUseCase`, all pub(super), defined in `crates/application/src/use_cases/cycle/qa_evidence.rs`:

It defines these evidence and visual-QA methods:

```rust
file_test_failure(&self, summary: &str) -> Option<TicketId>   // High "Tests failing:" Bug, deduped
collect_evidence(&self, ticket: &TicketId)                    // gated on deploy.host_port
collect_ui_evidence(&self, ticket: &TicketId, port: u16)
collect_api_evidence(&self, ticket: &TicketId, port: u16)
collect_missing_evidence(&self)                               // retries at most 2 tickets per cycle
visual_qa(&self, ticket: &TicketId, report: &mut CycleReport) // files up to 2 "UI:" Bugs
attach_test_case_screenshots(&self)
```

The visual-QA pass dispatches an AgentRequest with role PD and system prompt `system_prompt(PD)` from `crates/prompts.rs`, then parses only the last bracketed JSON slice and files via AddTicketUseCase.

## Configuration
The merge gate's screenshot comparison knobs live in `e2e/playwright.config.ts`:
- `expect.toHaveScreenshot.maxDiffPixelRatio` - default `0.02` (the 2% tolerance referenced in the failure comment).
- `expect.toHaveScreenshot.animations` - `disabled`.
- `use.viewport` - 1280x900; `use.colorScheme` - dark; `use.reducedMotion` - reduce.
- Suite-wide: fullyParallel false, workers 1, retries 0, timeout 30_000.
- webServer.command runs run-server.sh on port 4517 against a frozen fixture; baseURL points there.

CI workflow behavior lives inside `.github/workflows/visual-qa.yml`: trigger types [opened, synchronize], runner ubuntu-latest, timeout-minutes 30 (enforced by test), CARGO_TERM_COLOR=always env; artifact name template visual-qa-results-${{ github.sha }}.

For the in-agent side there is no config key that toggles individual pieces; the effective gate is deployment wiring:
| Condition | Effect |
|-----------|--------|
| Config.deploy.host_port unset | collect_evidence returns early (gate off); no evidence pass at all |
| host_port set + no ShotPort browser or storage | evidence recorded as kind "waived", never fatal |
| host_port set + ShotPort + ticket.has_ui | DoD UI screenshot uploaded + PD visual QA runs |

## Edge cases and limits
- **No GitHub-side auto bug filing.** The workflow only comments on failing PRs; it does not create issues. The planned ``gh issue create`` step exists only on unmerged commits (`a2fce61`, `4c7bc7a`) — do not document or rely on it as shipped.
- **Branch-protection enforcement is external and currently matches "Visual regression (screenshots)".** Nothing in this repo enables the rule; if it were configured against "Visual QA", it could never pass until CXA-B070's rename lands.
- **Single-job constraint.** The acceptance gate asserts exactly one job exists; adding a second job breaks passing_prs_get_a_green_check_and_no_comment's single green-or-red assumption.
- **Goldens never regenerate in CI.** --update-snapshots=off means a genuinely changed UI (intended redesign) fails until baselines are deliberately refreshed locally and committed.
- **Comment only on failure; artifacts always.** A thread-writing step must be failure()-gated so green runs post nothing.
- **Evidence capture best-effort/waved.** Missing browser/storage/probe/app-down records kind "waived" rather than blocking TEST. No host port means no evidence pass at all.
- **visual_qa needs PD cooperation.** Parses only the last bracketed JSON slice and caps at 2 defects; an unparseable review or failed engine run silently skips filing. No log noise proves nothing about coverage — absence of new UI bugs may just mean it skipped.
- **Debug-binary-only gate.** CI builds a debug binary (`cargo build --bin coxagent`); release-only regressions would not surface here.

## Code map
All paths verified by reading each file at HEAD:
- `.github/workflows/visual-qa.yml` — the Visual QA merge-gate workflow: single visual-regression job, build + Chromium install + snapshot-off suite + always() artifacts + failure()-gated comment script.
- `crates/app/tests/visual_qa_gate.rs` — acceptance tests parsing that workflow: the five public test fns plus YAML helpers load(), single_job(), steps(), command(), condition().
- `crates/application/src/use_cases/cycle/qa_evidence.rs` — in-agent DoD evidence and visual-QA methods: file_test_failure, collect_evidence, collect_ui_evidence, collect_api_evidence, collect_missing_evidence, visual_qa (files up to two 'UI:' Bugs via AddTicketUseCase), attach_test_case_screenshots.
- `crates/application/src/use_cases/cycle/mod.rs` — orchestration inside run_cycle_impl: calls collect_evidence / visual_qa / collect_missing_evidence / file_test_failure / attach_test_case_screenshots at their post-deploy and post-test points.
- `crates/application/src/use_cases/add_ticket.rs` — AddTicketUseCase::execute(AddTicketInput) used by both visual_qa and file_test_failure to create Bug tickets into the state store.
- `crates/app/src/builders.rs` — runtime wiring that constructs the ShotPort capture helper (shot), FilesPort (files), storage adapter, AgentEnginePort, and StateStorePort consumed by the evidence flow.
- `e2e/playwright.config.ts` — golden-screenshot tolerance (maxDiffPixelRatio 0.02), animations disabled, viewport/reduced-motion presets, workers 1 serial; webServer boots run-server.sh on port 4517 against a frozen fixture.
- `e2e/specs/*.spec.ts` (+ sibling *-snapshots dirs) — Playwright specs whose committed goldens back the Visual QA check.

## Related
Related tickets: CXA-F002 / CXA-F004 (original intent — catch UI drift from goldens); CXA-F005 (landed .github/workflows/visual-qa.yml in commit 6e25eaf); CXA-B063 (branch-protection required-check fix in commit a551d69). CXA-B070's job-display-name rename to exactly 'Visual QA' exists only on an unmerged branch at HEAD. The evidence layer also backs the F022 bug burn-down gate at crates/app/tests/burndown_f022_gate.rs. Transport/hand-off notes live in .claude/handoff-rest-runner.md.
