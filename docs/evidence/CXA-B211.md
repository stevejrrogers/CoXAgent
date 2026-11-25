# CXA-B211 — Closure evidence: hexagonal gate enforced in CI

**Status: CLOSED-BLOCKED — the gate is enforced in CI, but no Actions run can execute on this account (GitHub Actions billing). Nothing here is faked; every link below was verified to resolve and to show the state claimed.**

Recorded: 2026-09-13 · Final commit at record time: `ff25fd43` (`ff25fd43348a52d8b4465a84bab05696a0986eee`, main)

> Ticket-reference note: the closure ticket for this work ("record closure evidence and close with the gate enforced") names **CXA-B174**, which is the inactivity-watchdog ticket — the hexagonal-gate CI wiring and its proof artifacts belong to **CXA-B211** ("hexagonal gate wired into CI as a blocking build gate", subtask of the B182 final-proof chain). This record is filed under CXA-B211; the board should correct the reference on the closure ticket.

## 1. Guardrail command (the enforced one)

```bash
cargo test -p coxagent-app --test hexagonal_gate -- --nocapture
```

Local negative proof (from CXA-B211, recorded in the B211 wiki page): commit `d7ac5ae8` added a use case doing `std::fs::read_dir` directly; this exact command failed red naming `use_cases/gate_violation_fixture_b211.rs`; reverted in `f6482f0e`, gate green again after the revert.

## 2. Enforcement in CI — no bypass (verified against `.github/workflows/ci.yml` at `ff25fd43`)

- Wired by commit `660b65ab` ("ci(CXA-B211): wire the hexagonal gate into guard-tests as a blocking build gate").
- Runs as an ungated, always-on step of the merge-blocking `guard-tests` job (required check **"ownership + CI wiring guards"**):

  ```yaml
  - name: Hexagonal gate (CXA-B211 — application layer does no direct IO)
    run: cargo test -p coxagent-app --test hexagonal_gate -- --nocapture
  ```

- No skip/report-only flag exists: there is no `continue-on-error`, no `if:` on any gate job (the only conditionals in the file are the `failure()`/`always()` teardown steps of `deploy-smoke`), and `ci_availability_gate.rs` fails CI if a job or gate step is re-gated, renamed or dropped.

## 3. Run evidence — blocked externally (GitHub Actions billing)

Every Actions run on this private repo since ~2026-08-08 dies ~2s after trigger with **zero steps** and the annotation: *"The job was not started because recent account payments have failed or your spending limit needs to be increased."* Run `34761356136` was re-run by hand on 2026-09-13T14:23Z — same result, so this is not transient.

| Proof | Run / commit | Expected | Actual |
|---|---|---|---|
| Positive (branch dispatch) | [34759804489](https://github.com/stevejrrogers/CoXAgent/actions/runs/34759804489) on `feat/cxa-b211-guardrail-ci-gate` | green | all jobs not started (billing) |
| Positive (merge to main) | [34759884772](https://github.com/stevejrrogers/CoXAgent/actions/runs/34759884772) after `660b65ab` → main | green | all jobs not started (billing) |
| Negative (deliberate violation) | [34760026764](https://github.com/stevejrrogers/CoXAgent/actions/runs/34760026764) after `d7ac5ae8` | red, naming the file | all jobs not started (billing) |
| Final-commit runs | [34761356136](https://github.com/stevejrrogers/CoXAgent/actions/runs/34761356136) at `ff25fd43` (+ manual re-run 2026-09-13T14:23Z); [34762710149](https://github.com/stevejrrogers/CoXAgent/actions/runs/34762710149) at `8de7ce21` (this record) | green | all jobs not started (billing) |

For completeness: the last CI run that actually executed to a green conclusion is [31219380265](https://github.com/stevejrrogers/CoXAgent/actions/runs/31219380265) (`9f9b8a29`, 2026-08-07) — it **predates the gate wiring** and is not evidence for this ticket.

## 4. What remains to close this ticket

A repo-admin action outside the repo (diagnosed in the CXA-B211 wiki page and team memory):

1. Fix Billing & plans → payment method / spending limit on the `stevejrrogers` account (or make the repo public to get free-plan Actions minutes; branch protection is likewise unavailable on the current free plan).
2. Re-run `34761356136` (or push to main) and confirm all jobs execute and `guard-tests` is green on `ff25fd43` or later.
3. Update this record's table with the green run URL and flip the status line to CLOSED with the run link.

No source change is needed or wanted: the wiring is verified correct and the gate demonstrably fails red with a named file when a runner actually starts.
