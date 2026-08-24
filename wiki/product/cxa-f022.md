FOLDER: -
# Bug Burn-down for the F001 Prompt System

**Keywords:** F001, prompt system, bug burn-down, gating bugs, B1101, B1102, B1103, B1104, regression test evidence, RunTestUseCase, system_prompt, prompts_resolve

## Overview

CXA-F022 is the bug burn-down that un-gates feature **F001**, the prompt system. Four open bugs (**B1101–B1104**) each depend on F001 via their `depends_on`, so F001 cannot close until all four are cleared. The deliverable is *enforcement*, not four ad-hoc fixes: a bug only counts as burned down when its fix ships with its own recorded root-cause regression-test PASS in that ticket's QA evidence — never when the symptom was merely masked. It is for anyone who needs to know how "we fixed four bugs" is proven done against a checkable contract.

## How it works

A bug "gates feature F001" when it is type `Bug` and its dependency set contains id `F001`. That predicate is computed by `gating_bugs()` over a [`ProjectState`](crates/application/src/state/mod.rs). The acceptance fixtures create four such bugs — B1101–B1104 — each high priority with an F001 dependency added via `Ticket::add_dependency(Role::Sa, tid("F001"))`.

The burn-down is complete (`burndown_complete()`) iff all three hold:

1. Every gating bug has status `Verified`, and none remains Open or Fixed.
2. Each carries its own root-cause regression PASS in `ticket_evidence`: evidence label starts with `REGRESSION TEST`, detail contains both "reproduces" and "root cause", and contains none of the masking words "symptom", "workaround", or "incidentally".
3. No re-opened copy of an already-cleared bug exists — a new Open/Fixed bug sharing a cleared title still gates F001 and blocks closure.

**Production path.** AC#2/#3 are driven through the real use case (see `ac2_ac3_production_verification_records_regression_evidence`), not just fixtures. When [`RunTestUseCase::execute()`](crates/application/src/use_cases/run_test.rs) runs and a previously-`Fixed` bug does not resurface as a newly filed bug that pass (`passed`, run_test.rs), TEST promotes it to Verified *and* records provenance via `state.add_evidence(id.to_string(), "test", "REGRESSION TEST", "...root cause fixed at source.")`. Promoting without recording that evidence fails red-for-the-right-reason pre-F022.

**Prompt invariants.** AC#5 guards what F001 actually owns: every run composes its full system prompt as BASE + ENGINEERING_STANDARDS + role section through [`system_prompt(role)`](crates/application/src/prompts.rs). Order and membership must be unchanged by any fix; an optional per-project override seam ([`compose_role_system()`](crates/application/src/prompts_resolve.rs)) replaces only the role body while those two invariants survive (`g4`).

## Usage

Run the acceptance gate to see whether all four bugs are burned down:

```bash
cd crates/app
cargo test --test burndown_f022_gate
```

This runs all acceptance tests plus AC#5 and the production-path end-to-end check (`ac1_all_four_bugs_verified_and_none_open_or_fixed`, `ac2_ac3_production_verification_records_regression_evidence`, etc.). Run just one contract:

```bash
cd crates/app && cargo test --test burndown_f022_gate ac5_system_prompt_still_composes_base_standards_role
```

The g1–g4 fix contracts live in their own integration suite:

```bash
cd crates/app && cargo test -p coxagent-application prompts_resolve
```

The production-path proof drives real TEST verification against an in-memory store seeded with one Fixed gating bug:

```rust
// ac2_ac3_production_verification_records_regression_evidence() seeds state,
// runs RunTestUseCase against NoNewBugs ("[]"), then asserts:
assert_eq!(s.ticket(&id).status(), Status::Verified);   // promoted Fixed → Verified
assert!(recorded_regression_pass(&s, &id));             // own root-cause PASS recorded
```

## Interface

No public HTTP endpoints or CLI flags were added; CXA-F022 tightens existing domain logic. Relevant interfaces:

| Item | Location | Notes |
|------|----------|-------|
| `RunTestUseCase::execute()` | use_cases/run_test.rs:61 | Runs TEST agent; files discovered bugs; promotes Fixed→Verified + records REGRESSION TEST evidence |
| `.add_evidence(ticket_id_string(), kind, label, detail)` | state/mod.rs:437–450 | Appends to a bounded per-ticket list (max 6, oldest evicted); caps label at 120 chars and detail at 1200 chars |
| `.ticket_evidence` — `BTreeMap<String, Vec<Evidence>>` keyed by ticket id string | field on ProjectState (state/mod.rs:232) | Rendered by the dashboard / required by the inbox |
| struct [`Evidence { kind, label, detail }`](crates/application/src/state/work.rs) — `kind ∈ screenshot\|api\|test\|waived` | state/work.rs:224–231 |

Regression-evidence contract consumed by the gate (`recorded_regression_pass()`): label prefix must be `REGRESSION TEST`; detail must contain `PASS`, `reproduces`, and `root cause`, and must NOT contain any of `symptom`, `workaround`, or `incidentally`.

## Configuration

No configuration settings change CXA-F022's burn-down logic directly — it is hard-coded domain rules in the gate. Two existing knobs shape *when* promotion happens upstream of evidence recording:

| Setting | Default (`Config::default`) | Effect on verification path |
|---------|----------------------------|------------------------------|
| `deploy.host_port` | none (absent) | With a host port set, `gate_on = self.config.deploy.host_port.is_some()` becomes true (run_test.rs) and promotion defers until DoD evidence is attached to `.ticket_evidence[id]`. Absent port → no such deferral. |
| `workflow.human.gate_verify` (`HumanConfig`) | off (`false`) | When on (run_test.rs:187–194), TEST stops at "evidence attached" and a human renders the verdict from their inbox; the ticket stays Fixed until then. |

The burn-down acceptance criteria themselves are code constants in `burndown_f022_gate.rs`, not configurable data.

## Edge cases and limits

CXA-F022 deliberately does **not** perform code fixes for the prompt-system symptoms themselves beyond guarding composition; its scope is proving each gating fix has its own passing root-cause regression test.

How it fails:
- A Verified bug lacking its own recorded root-cause PASS → not counted cleared (**AC#2/#3**, including via production path).
- Only some of the four cleared → burn-down stays in progress (**AC#4a**: clearing just one asserts false).
- A re-opened copy sharing a cleared title appears → closure blocked even when all four originals are Verified (**AC#4b**, uses id B1199 titled "prompt defect B1101").
- Overwriting/masking language ("symptom", "workaround", "incidentally") disqualifies otherwise-PASSing detail.
- An empty gating-bug set reads as not-complete rather than done (`burndown_complete()` returns false when there are no gating bugs).

Limits:
- Evidence is bounded server-side to 6 per ticket with capped text (`add_evidence`, state/mod.rs:437–450), so old entries can be evicted once full.
- The fixture ids B1101–B1104 live only in this repo's tests today; there are no persisted project tickets for them.
- The F001 work lives on branch `feat/CXA-F022` (commit ec702c3 adds unit-test guards); it is **not** merged into current main.

## Code map

- crates/app/tests/burndown_f022_gate.rs — Executable acceptance criteria for every AC (#1–#5). Fixture factory (`gating_bug`, `fresh_state` seeding B1101–B1104), predicates (`gating_bugs`, `recorded_regression_pass`, `reopened_copy_exists`, `burndown_complete`), plus production-path doubles (`MemStore`, engine double).
- crates/application/src/prompts_resolve.rs — F001 override seam: role-key validation (`is_valid_role_key`), loader (`load_role_override`), pure precedence decision (`resolve_role_body`) and composer that preserves BASE + ENGINEERING_STANDARDS invariant (`compose_role_system`) — where g1–g4 root causes are fixed.
- crates/application/tests/prompts_resolve.rs — g1–g4 regression tests over that resolver (missing override → embedded default; stray key rejected; blank counts absent; invariants survive role replacement).
- crates/application/src/prompts.rs — Embedded defaults BASE / ENGINEERING_STANDARDS / role constants and [`system_prompt(role)`](crates/application/src/prompts.rs) (:343). Branch feat/CXA-F022 adds a dedicated unit module here pinning composition.
- crates/application/src/use_cases/run_test.rs — Production TEST loop that promotes Fixed→Verified while recording REGRESSION TEST provenance (:195–213).

## Related

Other pages touching adjacent surface:
- product/cxa-f021 (this space) — config schema-drift gate over project config fields such as the host port that upstream gates `gate_on` in run_test.rs.

Related code and tickets:
- The same prompt-composition invariant is asserted in an integration test over the resolver path: crates/application/tests/prompts_resolve.rs (`g4_base_and_engineering_invariants_survive_even_when_role_replaced`) pins BASE first then ENGINEERING_STANDARDS. AC#5 (burndown_f022_gate.rs) pins the identical composition on `system_prompt`.
- Implementation commit for F022's regression guards: `ec702c3` on branch `feat/CXA-F022` (adds a dedicated unit-test module to prompts.rs). Not yet merged to main.

