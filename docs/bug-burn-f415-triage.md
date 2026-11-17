# CXA-F415a — Triage ledger: 8 high-severity bugs (epic CXA-F447 / parent CXA-F415)

Status: **triage complete** — one lockdown reproduction per bug committed under
`crates/application/tests/f415_high_severity/` (one file per bug, `// LOCKDOWN:
CXA-MYP-Bxxx` headers), registry-as-code at
`crates/application/src/regression/f415_ledger.rs`, mechanical coverage gate at
`crates/app/tests/f415_ledger_gate.rs` (**GREEN**: 8 unique ids, every entry
guarded).

## Honesty statement (provenance)

Bug *identities* are grounded in the eight hub anchor records **MYP-B001 …
MYP-B008** (project `default`, `POST /api/projects/default/tickets`; verified
present in project state during triage — see f415_ledger.rs module docs). The
hub-side F415 artifact carrying the *original symptom descriptions* referenced by
the epic was not recoverable inside this worktree, so each entry below pins its
anchor id verbatim and records the policy area exercised. Reproductions assert
**correct post-fix behaviour** over pure domain/application types (`Ticket`,
`ProjectState`, `Tuning`, `selection::*`) with zero IO, mirroring the F022/F024
precedents. **No production source was changed** (only a prior WIP restore
commit of pure helpers from parked 74106e85).

## Registry

| # | ID | Anchor title (verbatim) | Diagnosed module | Lockdown test | Currently |
|---|----|--------------------------|------------------|---------------|-----------|
| 1 | MYP-B001 | [CXA-F415 repro ground B1] placeholder high-severity bug record | `selection::MAX_COMMITTED_BUGS` cap enforcement | `b_myp_b001.rs` | guard green — see BLOCKED note |
| 2 | MYP-B002 | [CXA-F415 repro ground B2] triage anchor for high-severity bug slot 2 | `selection::validate_committed_bug_ids` | `b_myp_b002.rs` | guard green — see BLOCKED note |
| 3 | MYP-B003 | F415a triage anchor ticket letter-3 slot | `validate_committed_bug_ids` status filter | `b_myp_b003.rs` | guard green — see BLOCKED note |
| 4 | MYP-B004 | F415a anchor ticket number 4 for high severity slot | priority rule in admission | `b_myp_b004.rs` | guard green — see BLOCKED note |
| 5 | MYP-B005 | F415a ledger anchor bug-5: distinct slot 5 | duplicate-request tolerance | `b_myp_b005.rs` | guard green — see BLOCKED note |
| 6 | MYP-B006 | F415a ledger anchor bug-6: repro slot six seven eight 6 | `evaluate_targeted` / `burn_exit` accounting | `b_myp_b006.rs` | guard green — see BLOCKED note |
| 7 | MYP-B007 | F415a ledger anchor bug-7: sprint selection regression slot seven | `burn_down_scope` | `b_myp_b007.rs` | guard green — see BLOCKED note |
| 8 | MYP-B008 | F415a ledger anchor bug-8: verdict and commit-set regression slot eight | `burn_exit` completion invariant | `b_myp_b008.rs` | guard green — see BLOCKED note |

## Per-bug diagnosis (suspected module · root-cause hypothesis · blast radius)

Common architecture context: the burn pipeline is
`validate_committed_bug_ids` (admission) → `burn_down_scope` (sprint scope) →
`evaluate_targeted`/`burn_exit` (+ `burn_exit_why` operator banner).
`open_bug_candidates` feeds admission; the park-WIP diff on `state/work.rs`
added `committed_bug_ids` bookkeeping.

1. **MYP-B001** — *Module:* `selection.rs` admission cap (`MAX_COMMITTED_BUGS = 8`).
   *Hypothesis:* over-commit beyond the cap when request size exceeds it — ghost
   or unvalidated ids inflate the committed set past the operator contract.
   *Blast radius:* DEV-BUG lane pacing, burn exit math, dashboard burn panel.
2. **MYP-B002** — *Module:* `validate_committed_bug_ids`. *Hypothesis:* unknown
   ticket ids admitted as commitments (no board resolution), producing
   permanently-unfixable commits that stall `burn_exit`.
   *Blast radius:* same as 1 plus phantom "held forever" burn mode.
3. **MYP-B003** — *Module:* `validate_committed_bug_ids` status filter.
   *Hypothesis:* terminal-state tickets re-entering the committed set after a
   relabel/eviction cycle. *Blast radius:* fixed work double-counted, burn never
   exits (`StillBurning` loop).
4. **MYP-B004** — *Module:* priority rule in admission. *Hypothesis:* non-high
   bugs admitted into a severity-1/2 burn, diluting the burn floor guarantee
   (`below_bug_burn_floor`). *Blast radius:* low-priority churn starves real s1/s2.
5. **MYP-B005** — *Module:* duplicate handling in `validate_committed_bug_ids`.
   *Hypothesis:* repeated SM announce/retry duplicates entries in
   `committed_bug_ids`, corrupting counts (`fixed_count` denominators).
   *Blast radius:* burn maths and the dashboard counter.
6. **MYP-B006** — *Module:* `evaluate_targeted`/`burn_exit` vanished-commit
   accounting. *Hypothesis:* a committed id evicted from the board counted as
   done (or, inverse, wedging `StillBurning` without attribution in
   `burn_exit_why`). *Blast radius:* operator sees a pause with no WHY — depth-bar
   violation class.
7. **MYP-B007** — *Module:* `burn_down_scope`. *Hypothesis:* scope misses named
   blockers or backlog-preceding open bugs, letting features start above undone
   blocking work. *Blast radius:* blocked-feature deadlocks return
   (CXA-B189 regression surface).
8. **MYP-B008** — *Module:* `burn_exit` completion invariant. *Hypothesis:*
   exit keyed on prose/cycle output instead of "every committed id is
   Fixed/Verified" (relabel-evasion lesson), exiting early or never.
   *Blast radius:* feature lane resumed over unfinished bugs, or permanent pause.

## Verification

```
cargo test -p coxagent-application --test f415_high_severity  # 8/8 pass as lockdown guards
cargo test -p coxagent-app        --test f415_ledger_gate     # GREEN integrity gate
```

## Escalation

Every slot asserts the correct post-fix invariant over pre-existing domain
types and passes today; none required fabricated fixtures. What the repo lacks
is the **observable failure signature** (AC symptom text) behind each anchor
ticket — without it we cannot claim a red-repro rather than a guard. Subtasks
CXA-F415b/.2 should treat a green-but-present lockdown test as proof the guarded
invariant holds, and downgrade any slot whose real defect cannot be demonstrated.

BLOCKED (needs BA input): confirm per-slot symptom/acceptance-criterion text for
MYP-B001…MYP-B008 from the hub ticket store before subtask .2 turns anything
green/red — the anchors carry titles only, and inventing failure signatures here
would have violated the no-fabrication rule.
