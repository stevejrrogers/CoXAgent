# Visual QA

Automated visual regression gating for Pull Requests targeting `main`. The
pipeline builds `coxagent`, runs the Playwright screenshot suite against committed
baselines on port 8101, files a Bug on regression, and comments back with ticket
links.

## Approving baseline updates

When a diff is an intentional design change rather than a regression, an approved
baseline replacement happens by commenting `/visual-qa-update` on the PR (or by
applying the `visual-qa:update-baseline` label). This triggers committing updated
Playwright snapshots back to the branch.

## Local baseline refresh

Update committed snapshots locally with the e2e package script:

```sh
cd e2e && npm run baseline
```

