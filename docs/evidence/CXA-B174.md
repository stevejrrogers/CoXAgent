# CXA-B174 — Closure evidence: regression guardrails + hexagonal gate enforced in CI

**Status: NOT YET CLOSEABLE — blocked by the same GitHub Actions billing failure as CXA-B211 (SA ruling: account fix first; see ASK below).** Everything verifiable in-repo is recorded here; the single missing item is one green enforced CI run on the final commit, which the repo cannot produce until the account-level billing block is lifted.

## 1. Ticket

- **CXA-B174** — "Final build-gate proof: wire the guardrail into CI and close with recorded evidence" (parent CXA-B167; siblings CXA-B181 regression guardrails, CXA-B211 CI wiring, CXA-B182 build-gate proof, CXA-B212 record closure evidence).
- Scope honoured: documentation + CI enforcement + one final green run. **No guardrail source changes** — this ticket touches only this evidence file and ticket state.

## 2. Guardrail command (the enforced gate)

```
cargo test -p coxagent-app --test hexagonal_gate -- --nocapture
```

Wired verbatim into `.github/workflows/ci.yml`, job `fmt · clippy · test`, step `Hexagonal gate (CXA-B211 — application layer does no direct IO)` (ci.yml:102-103). Verified in-tree at commit `690b32c6a0f96cd880fb7bd7127aa9d2d5de1d9d` and re-verified unchanged at `fef6c5134aaf8b64e238d33834cb7a8251500c7c` (current main tip at record time):

- **No skip / report-only / continue-on-error bypass exists.** `grep` over `ci.yml` and `crates/` for `continue-on-error`, `report-only`, `report_only`, `SKIP_HEX`, `allow_skip` returns nothing; the step is a plain `run:` that fails the job on a red gate.
- The gate itself is real: `crates/app/tests/hexagonal_gate.rs` fails any new application-layer file that does IO outside a port, and its grandfather list only shrinks.

## 3. Run evidence

| Run | Commit | Trigger | Result | Evidence |
|---|---|---|---|---|
| 34761356136 | 8de7ce21 | push | **failure (billing)** | https://github.com/stevejrrogers/CoXAgent/actions/runs/34761356136 |
| 34762777073 | 690b32c6 | push, then manual re-run 14:45:01Z | **failure (billing)** | https://github.com/stevejrrogers/CoXAgent/actions/runs/34762777073 |
| 34764405594 | fef6c513 (current main tip) | push 15:02:33Z; manual re-runs 15:36Z, 15:43Z, 15:45Z (attempts 2–4) | **failure (billing), 4/4 attempts** | https://github.com/stevejrrogers/CoXAgent/actions/runs/34764405594 |
| **PENDING** | _final commit_ | push | **green (required for closure)** | _to be recorded here_ |

Every run since ~2026-08-08 dies ~2s in before any job starts, with the annotation: *"The job was not started because recent account payments have failed or your spending limit needs to be increased."* The 14:45Z re-run of 34762777073 failed identically — this is current, not stale, evidence. Jobs therefore never execute, so the gate has not yet been observed **executing** green in Actions (per the team lesson: verify a check *fired*, not that it is written).

Locally, on this worktree, the gate passes (source of truth for the test itself):

```
cargo test -p coxagent-app --test hexagonal_gate
```

## 4. Blocker & owner (already escalated and answered)

SA answered (CXA-B211): the fix is **account-level and outside the repo** — no in-repo ticket can do it. Owner: **repo admin / account holder `stevejrrogers`**. Required action: fix billing / raise the spending limit (or make the repo public on the free plan), then re-run 34762777073 — or push this branch — and record the green run URL in the table above, then flip CXA-B174 (and CXA-B212) to closed with this file as the evidence link.

`ASK SA: Actions remain billing-blocked (re-run 34762777073 at 14:45Z failed with the same spending-limit annotation) — after you lift the billing block, should the closing agent re-run 34762777073 on 690b32c6 or push a fresh commit, and may this evidence file be committed as-is with the green-run row marked PENDING until then?`
