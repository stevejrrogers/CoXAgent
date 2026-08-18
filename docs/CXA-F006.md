FOLDER: Engineering

# Visual QA Merge Gate

**Keywords:** Visual QA merge gate, required status check, branch protection main, golden screenshots, visual regression CI, Playwright toHaveScreenshot, maxDiffPixelRatio 0.02, update-snapshots=off, visual_qa_gate.rs

## Overview

CXA-F006 makes pixel-level visual regression a hard merge gate on protected `main`: GitHub Actions workflow `.github/workflows/visual-qa.yml` (job display name **Visual QA**) runs Playwright against committed golden screenshots on every pull request targeting main; GitHub branch protection lists **Visual QA** as a required status check so its red/green outcome blocks merges outright. It serves developers who change rendering code (the failing-PR comment names exactly which specs drifted) and anyone operating CI or GitHub branch-protection settings.

## How it works

The gate reduces to one workflow plus one Rust guard test; both exist at HEAD in the CoXAgent project (`/Users/luton/projects/CoxAgent`).

Workflow `.github/workflows/visual-qa.yml`:

1. **Trigger.** `on.pull_request.types: [opened, synchronize]`. There is no push trigger and no manual `workflow_dispatch`.
2. **Single job.** One job `visual-regression` with display name exactly **Visual QA**, on `ubuntu-latest`, `timeout-minutes: 30`, permissions `contents: read`, `issues: write`, `pull-requests: write`.
3. **Build.** Step "Build coxagent (debug binary used by e2e webServer)" runs `cargo build --bin coxagent`.
4. **Browsers.** Step under working-directory e2e runs `npx playwright install chromium --with-deps`, installing headless Chromium plus its system packages.
5. **Run.** Step under working-directory e2e:
   ```sh
   npx playwright test --update-snapshots=off \
     --reporter=json > .playwright-report.json 2> .playwright-stderr.txt
   ```
   The shell block captures exit code (`set +e ... exit "$code"`) so any spec over tolerance turns the whole job red; snapshots are compared against committed goldens and never regenerated in CI.
6. **Artifacts.** An unconditional step (`if: always()`) uses actions/upload-artifact@v4 to upload artifact `visual-qa-results-${{ github.sha }}` from paths e2e/test-results/ and e2e/.playwright-report.json — diff images stay inspectable even when the run passes.
7. **PR comment.** A failure-only step (`if: failure()`) uses actions/github-script@v7 to read `.playwright-report.json`, count failed tests ("N test(s) exceeded the 2% screenshot tolerance"), list each failing spec title as bullets linking to run artifacts via issues.createComment; if the report is missing or unparseable it posts generic text pointing at artifacts.

Guard test [`crates/app/tests/crates/app/tests`](/Users/luton/projects/) pins these invariants by parsing visual-qa.yml into serde_yaml::Value:

