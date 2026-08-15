FOLDER: -
# Bug Burn-down: Clearing the F001 Prompt System's Gating Bugs (CXA-F022)

**Keywords:** F001, prompt system, bug burn-down, gating bugs, B1101, B1102, B1103, B1104, regression test evidence, RunTestUseCase, system_prompt

## Overview

CXA-F022 is the bug-burn-down that un-gates feature **F001** (the prompt system) by clearing four open bugs that depend on it — **B1101, B1102, B1103, B1104**. It is for anyone who needs to know how a burn-down is proven done: it turns "we fixed four bugs" into a checkable contract. The real deliverable is *enforcement*: a bug only counts as cleared when its fix ships with its own recorded root-cause regression test PASS in that ticket's QA evidence — never when the symptom was merely masked.

## How it works

F001 is gated by any `Bug` ticket whose `depends_on` includes F001 (`gating_bugs()` in `crates/app/tests/burndown_f022_gate.rs:48`). The four fixtures are **B1101–B1104**, each created high-priority with dependency on F001 via `Ticket::add_dependency(Role::Sa, tid("F001"))`. They represent prompt-system defects; CXA-F022's work is not four code fixes but the verification loop that proves each one closed.

The burn-down is complete (`burndown_complete()`, line 91) iff **all three** hold:

1. Every gating bug has status `Verified` (and none remains `Open` or `Fixed`).
2. Every one carries its own root-cause regression PASS in `ticket_evidence`: label starts with `REGRESSION TEST`, and detail contains `PASS`, `reproduces`, and `root cause`, while containing none of the masking words `symptom`, `workaround`, or `incidentally`.
3. No re-opened copy of an already-cleared bug exists — a new Open/Fixed bug sharing a cleared title still gates F001 and blocks closure.

**Production path.** The acceptance criteria are driven through the real use case in `RunTestUseCase::execute` (`crates/application/src/use_cases/run_test.rs:61`). When TEST runs and a previously-`Fixed` bug does not resurface as a newly filed bug this pass (`passed`, lines 159–168), TEST promotes it to Verified (line 196) *and* records provenance via `state.add_evidence(id, "test", "REGRESSION TEST", "...PASS on current master... root cause fixed at source.")` (lines 205–211). That recorded evidence is exactly what AC#2/#3 demand; promoting without recording it fails the gate red-for-the-right-reason.

**Prompt invariants.** AC#5 guards what F001 actually owns: every run still composes its full system prompt as BASE + ENGINEERING_STANDARDS + role section via `system_prompt(role)` (`prompts.rs:343`) — order and membership unchanged by any fix.

## Usage

Run the acceptance gate to see whether all four bugs are burned down:

```
cd crates/app
cargo test --test burndown_f022_gate
```

This runs all acceptance tests plus AC#5 and the production-path end-to-end check:
`ac1_all_four_bugs_verified_and_none_open_or_fixed`, `ac2_ac3_production_verification_records_regression_evidence`, `ac4_subset_leaves_burn_down_in_progress`, and the others. Run just AC#5 or the F022 unit guards directly:

```
cd crates/app && cargo test --test burndown_f022_gate ac5_system_prompt_still_composes_base_standards_role
cargo test -p coxagent-application prompts::system_prompt_composition_tests   # unit guards added in commit ec702c3
```

Production-path end-to-end proof (drives real TEST verification against an in-memory store with one Fixed gating bug):

```rust
// ac2_ac3_production_verification_records_regression_evidence() seeds state,
// runs RunTestUseCase against NoNewBugs engine ("[]" stdout), then asserts:
assert_eq!(s.ticket(&id).status(), Status::Verified);       // promoted
assert!(recorded_regression_pass(&s, &id));                 // evidence recorded
```

## Interface

No public HTTP endpoints or CLI flags were added; this ticket tightens existing domain logic. Relevant interfaces:

| Item | Location | Notes |
|------|----------|-------|
| `RunTestUseCase::execute()` | run_test.rs:61 | Returns filed ids; promotes Fixed→Verified + records regression evidence |
| `.add_evidence(ticket, kind, label, detail)` | state/mod.rs:437 | Appends to bounded per-ticket list (6 max); used by TEST at run_test.rs:205 |
| `.ticket_evidence` | state/mod.rs:232 | `<TicketIdString>` → Vec\<Evidence\>; rendered by dashboard / required by inbox |
| struct Evidence { kind, label, detail } | state/work.rs:224 | kind ∈ screenshot\|api\|test\|waived |
| `.post_comment(author, body)` | state/mod.rs:625 | Deferral notes for missing evidence / human gate |

Regression-evidence contract consumed by the gate (`recorded_regression_pass()`, burndown_f022_gate.rs:59):
label prefix = "REGRESSION TEST"; detail must contain "PASS", "reproduces", "root cause"; must NOT contain "symptom"/"workaround"/"incidentally".

## Configuration

No configuration settings change CXA-F022's behaviour directly. Two existing knobs shape *when* promotion happens upstream of evidence recording:

| Setting | Default | Effect on verification path |
|---------|---------|----------------------------|
| None for CXA-F022 specifically | — | Burn-down logic is hard-coded domain rules |
| workflow.human.gate_verify (`Config`) | off (`Config::default`) | If on at run_test.rs:187–194 — promotion pauses at Fixed awaiting human verdict even after REGRESSION TEST evidence attaches |

The deploy-based DoD deferral also checks existence of any entry in `.ticket_evidence[id]` before promoting (run_test.rs:176).

## Edge cases and limits

This ticket deliberately does **not** perform code fixes for symptoms themselves beyond guarding composition; its scope is proving each gating fix has its own passing root-cause regression test.
How it fails:
- A Verified bug lacking its own recorded root-cause PASS → not counted cleared (**AC#2/#3**).
- Only some of the four cleared → burn-down stays in progress (**AC#4a**: clearing just one asserts false).
- A re-opened copy sharing a cleared title appears → closure blocked even when all four originals are Verified (**AC#4b**, uses id B1199 titled "prompt defect B1101").
- Overwriting/masking language ("symptom", "workaround", "incidentally") disqualifies otherwise-PASSing detail.
Evidence is bounded server-side to 6 per ticket with capped text (`add_evidence`, state/mod.rs:437–449), so old entries can be evicted once full.
The fixture ids live only in this repo's tests; there are no persisted project tickets for them today.

## Code map

- crates/app/tests/burndown_f022_gate.rs — Executable acceptance criteria for all ACs; fixture factory (`gating_bug`, `fresh_state` with B1101–B1104), predicates (`gating_bugs`, `recorded_regression_pass`, `reopened_copy_exists`, `burndown_complete`), plus production-path store (`MemStore`) / engine (`NoNewBugs`) doubles.
- crates/application/src/use_cases/run_test.rs — Production TEST loop; promotes Fixed→Verified and records REGRESSION TEST provenance (lines 195–213) satisfying AC#2/#3 through the real use case.
- crates/application/src/prompts.rs — BASE (:6), ENGINEERING_STANDARDS (:60), role-section constants PO/SM/BA/SA/TEST (:106+:243), and system_prompt (:343) whose BASE + standards + role composition AC#5 protects; carries F022's own unit-test module `system_prompt_composition_tests` added by commit ec702c3 at :1622+.

## Related

Other wiki pages that touch adjacent surface:
- product/cxa-f021 (in this space) — config schema-drift gate over project config, the same source processed alongside prompt rollout context.

Related code:
- The same prompt-composition invariant is asserted in an integration test over the resolver path: crates/application/tests/prompts_resolve.rs:150 ("F001 invariant: BASE first, then ENGINEERING_STANDARDS"). AC#5 (burndown_f022_gate.rs:229) and the F022 unit module both pin the identical composition on `system_prompt`.
- Related ticket chain referenced by this burn-down's mechanics: CXA-F021 (this space), sharing project-config surface. Implementation commit for F022's regression guards: ec702c3 on branch feat/CXA-F022 (not yet merged to main).

