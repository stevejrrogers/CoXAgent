# CXA-B211 — Hexagonal gate wired into CI as a blocking build gate

**Keywords:** hexagonal gate, CI, ci.yml, guard-tests, blocking gate, CXA-B210, hexagonal_gate.rs, negative proof, Actions billing blocker

## What landed (merged to main)

- **One CI-file change, commit `660b65ab`** ("ci(CXA-B211): wire the hexagonal gate into guard-tests as a blocking build gate"): inside the existing merge-blocking job **`guard-tests`** (required check **"ownership + CI wiring guards"**, required via `.github/required_checks.yml`, enforced on PRs per team convention) in `.github/workflows/ci.yml`, a new ungated step runs the CXA-B210-proven command verbatim:

  ```yaml
  - name: Hexagonal gate (CXA-B211 — application layer does no direct IO)
    run: cargo test -p coxagent-app --test hexagonal_gate -- --nocapture
  ```

  No triggers, jobs, or required-check names were added or changed — so the CI wiring guards (`ci_availability_gate.rs`, 26 tests) and deploy smoke (`deploy_smoke.rs`, 19 tests) stayed green, verified locally before push and re-verified on main after the revert.

## Why it blocks merges

`guard-tests` is not gated behind the `changes:` filter and is a required check; a failing step fails the job, fails the PR check "ownership + CI wiring guards", and blocks merge. On a violation, cargo prints the failing test plus the gate's assertion message, which **names the offending file(s) and the rule** (put IO behind a port in `ports/outbound/`, see `GitPort::working_tree`), not just an exit code.

## Proofs — triggered, but dead at job startup (external blocker)

Every job in every run 2026-09-06→today dies ~2s in with **zero steps** and one annotation: *"The job was not started because recent account payments have failed or your spending limit needs to be increased"* — Actions billing/quota on this private repo, first diagnosed in team memory (four prior runs, same signature). Runs below are the proof artifacts; they cannot go green until billing is fixed (repo-admin action, outside this ticket).

| Proof | Run / commit | Expected result | Actual |
|---|---|---|---|
| Positive (branch dispatch) | [34759804489](https://github.com/stevejrrogers/CoXAgent/actions/runs/34759804489) on `feat/cxa-b211-guardrail-ci-gate` | green | all jobs not-started (billing) |
| Positive (merge to main) | [34759884772](https://github.com/stevejrrogers/CoXAgent/actions/runs/34759884772) after `660b65ab` → main | green | all jobs not-started (billing) |
| Negative | [34760026764](https://github.com/stevejrrogers/CoXAgent/actions/runs/34760026764) after `d7ac5ae8` | red naming the file | all jobs not-started (billing) |

**Negative proof, verified locally instead:** commit `d7ac5ae8` added a fake use case doing `std::fs::read_dir` directly. Running the exact CI command (`cargo test -p coxagent-app --test hexagonal_gate -- --nocapture`) failed with:

```
these application files reach for std::process/std::fs directly — put the IO behind a
port in ports/outbound/ (see GitPort::working_tree for the pattern) ...:
["use_cases/gate_violation_fixture_b211.rs"]
```

The revert is visible in history: `d7ac5ae8` → `f6482f0e` ("Revert \"test(CXA-B211): NEGATIVE PROOF …\""), and the guard is green again locally after the revert (`test result: ok. 1 passed`).

## Blocker to clear before re-verification

Fix GitHub Actions billing (Billing & plans → payment method / spending limit) as repo admin, then re-run the three runs above (or push anything to main). Nothing in the repo needs to change: the wiring is correct and the gate demonstrably fails red with a named file when the runner actually starts.

## Unmerged branch

`feat/cxa-b211-guardrail-ci-gate` contains the CI change and is already merged to main via fast-forward; it can be deleted after the post-billing re-verification.
