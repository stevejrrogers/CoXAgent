FOLDER: -
# Bug Burn-down — Clearing Open Bugs Gating F001

**Keywords:** burndown, gating bug, depends_on, regression test, root cause, QA evidence, Verified status, prompt system override, prompts_resolve, system_prompt

## Overview

CXA-F022 is a burn-down ticket: it clears four open bugs that gate the F001 prompt-system feature from shipping. It establishes a proof standard — an open gating bug is not burned down just because its ticket reaches `Verified`; it must also carry recorded QA evidence of a passing *root-cause* regression test in its own provenance (never a symptom mask or workaround probe). This page documents that standard, who enforces it (the TEST agent's verification path), and the four contracts (`g1`–`g4`) the fixes must preserve on the F001 prompt system. It is for engineers fixing or re-opening gating bugs and for any agent asked to verify whether a burn-down is actually complete.

## How it works

A bug **gates** feature F001 when its `depends_on()` contains `F001`. Burn-down is not a separate orchestrator use case — it falls out of the normal TEST verification loop once per cycle:

1. **Filing.** `RunTestUseCase::execute()` runs the TEST agent against the build and files new bugs via `AddTicketUseCase`, deduped by title.
2. **Fix.** DEV takes each gating bug from `Open` → `InProgress` → `Fixed`.
3. **Regression evidence.** When TEST promotes a still-`Fixed` bug that did NOT resurface as a newly filed bug this run (so its root-cause fix held), it records QA provenance before marking it Verified via [`state.add_evidence(id, "test", "REGRESSION TEST", "PASS on current master; ... root cause fixed at source.")`](run_test.rs). This is the CXA-F022 AC#2/#3 production change.
4. **Verify.** The transition to `Status::Verified` goes through [`Ticket::transition_to(Role::Test, Status::Verified)`](crates/domain/src/ticket.rs).
5. **Gate check.** Completion requires every gating bug be both Verified AND carry that root-cause regression PASS in its evidence — and no *re-opened copy* of an already-cleared bug exists.

The gate's acceptance logic lives in [`crates/app/tests/burndown_f022_gate.rs`](burndown_f022_gate.rs): helpers `gating_bugs()`, `recorded_regression_pass()`, `reopened_copy_exists()`, and the composite predicate [`burndown_complete()`](burndown_f022_gate.rs) encode AC#1–AC#4b as executable invariants over repo state.

Separately from burn-down mechanics, AC#5 guards the F001 surface itself: every run still composes `BASE + ENGINEERING_STANDARDS + role section` via [`prompts::system_prompt(role_section)`](crates/application/src/prompts.rs) with blank-line separation and each section appearing exactly once. The role-body override seam lives in [`crates/application/src/prompts_resolve.rs`](prompts_resolve.rs); resolution is a pure function over data read through the files port (`WorkspaceFilesPort`) — no direct IO.

## Usage

To clear one gating bug end-to-end as DEV/TEST do:

```text
Open      --DEV claims-->  InProgress  --DEV fixes-->  Fixed
Fixed     --TEST sees no resurfacing-->  records REGRESSION TEST PASS -->  Verified
```

On real repo state:

```rust
// A gating bug carries depends_on => F001.
let mut b = Ticket::new(
    TicketId::new("B1101").unwrap(), TicketType::Bug,
    "prompt defect B1101", "blocking F001",
    Priority::High, Complexity::Medium,
    /* has_ui */ false,
).unwrap();
b.add_dependency(Role::Sa, TicketId::new("F001").unwrap()).unwrap();

// Fix then prove at source:
b.transition_to(Role::DevBug, Status::InProgress).unwrap();
b.transition_to(Role::DevBug, Status::Fixed).unwrap();
state.add_evidence(&b.id().to_string(), "test", "REGRESSION TEST",
    "PASS on current master; regression test fails on pre-fix code \
     and reproduces cleanly; root cause fixed at source.");
b.transition_to(Role::Test, Status::Verified).unwrap();
```

Burn-down completes only when *every* gating bug meets that pattern; one still-open or re-opened copy keeps it in progress.

Driving through production rather than hand transitions uses the real use case against an in-memory store (see AC#2/#3):

```rust
let uc = RunTestUseCase::new(Arc::clone(&store), Arc::new(NoNewBugs),
                             Config::default(), PathBuf::from("/tmp"));
uc.execute().await?;
```

## Interface

Pure predicates defining burn-down completion (in [burndown_f022_gate.rs]):

| Function | Returns | Meaning |
|----------|---------|---------|
| `gating_bugs(state)` | Vec\<TicketId\> | Bugs whose `depends_on()` contains gate key (`GATED_FEATURE = "F001"`) |
| `recorded_regression_pass(state,&id)` | bool | Evidence label starts with REGRESSION_EVIDENCE_LABEL ("REGRESSION TEST"), detail contains REGRESSION_PASS_MARKER ("PASS") plus reproduction + root-cause wording; rejects detail mentioning symptom / workaround / incidentally |
| `reopened_copy_exists(state)` | bool | An Open/Fixed gating bug whose title equals an already-Verified cleared one |

Constants: REGRESSION_EVIDENCE_LABEL = "REGRESSION TEST"; REGRESSION_PASS_MARKER = "PASS"; GATED_FEATURE = "F001".

Evidence model:

- Producer: [`ProjectState::add_evidence(ticket: &str, kind: &str, label: &str, detail: &str)`](crates/application/src/state/mod.rs) pushes an `Evidence` record into ``ticket_evidence`` : BTreeMap\<String /*ticket id*/, Vec\<Evidence\>\>. Each ``Evidence`` holds { label : String , detail : String }.
- Production writer: inside ``RunTestUseCase.execute()`` ([run_test.rs] lines ~236–242), immediately before transitioning to Verified.
- Consumer predicates match those exact label/detail strings together with presence of both reproduction-signal words ("reproduces", "root cause") and absence of masking keywords ("symptom", "workaround", "incidentally").

Prompt-surface functions guarded by AC#5 ([prompts]/[prompts_resolve]):

```rust
pub fn system_prompt(role_section: &str) -> String                  // "{BASE}\n\n{ENGINEERING_STANDARDS}\n\n{role_section}"
pub async fn compose_role_system(files,...,work_dir,...,role_key) -> String      // BASE + standards + resolved body
pub async fn load_role_override(files,...,work_dir,...,role_key) -> Option<String>
pub fn resolve_role_body<'a>(embedded,&Option<&'a str>) -> Cow<'a str> // pure precedence decision
pub fn is_valid_role_key(key: &str) -> bool                        // KNOWN_ROLE_KEYS membership check
```

Role constants embedded defaults live alongside ``system_prompt`` ([prompts]) (`BASE`, ENGINEERING_STANDARDS plus per-role bodies PO/SM/BA/SA...).

## Configuration

CXA-F022 introduces no configuration settings. Burn-down correctness comes from data invariants encoded directly in test predicates and in the production promotion path, not from runtime switches; workflow flags that influence whether a bug can reach Verified predate this ticket (`WorkflowConfig.deploy.host_port` gates DoD-evidence deferral; ``WorkflowConfig.human.gate_verify`` defers to human verdict). Override loading uses only existing plumbing — nothing reads disk directly outside ``WorkspaceFilesPort``.

## Edge cases and limits

Verified boundary conditions:

- **Masked fix rejected even when status moves** (AC#2/#3): promoting a bug to ``Verified`` WITHOUT recording its own passing root-cause regression PASS never counts as burned down — reaching Verified is necessary but not sufficient.
- **Subset is not done** (AC#4a): clearing only some of the gating bugs leaves the burn-down in progress; ``burndown_complete()`` requires the whole set.
- **Re-opened copy blocks closure** (AC#4b): even when all four originals are Verified, a new Open/Fixed gating bug sharing an already-cleared title keeps closure from happening.
- **Empty set is not completion**: ``burndown_complete()`` returns false when there are no gating bugs at all.
- **Prompt composition must stay intact** (AC#5): any prompt-system fix that drops/reorders BASE / ENGINEERING_STANDARDS / role section breaks every role calling ``system_prompt``; override files for unknown roles are rejected (`g2`), blank bodies count as absent (`g3`), embedded defaults win on missing file (`g1`) while BASE + standards survive even when only one ROLE body is replaced (`g4`) — see [tests/prompts_resolve.rs].

Deliberately NOT done here: F022 adds no dedicated burn-down orchestrator or endpoint — it enforces through existing paths plus gate tests; it does not auto-file or auto-triage new bugs.

## Code map

These paths implement CXA-F022 and its acceptance gate:

- `crates/app/tests/burndown_f022_gate.rs` — acceptance gate written before implementation so its criteria are pinned as executable invariants over repo state: helpers `gating_bugs()`, `recorded_regression_pass()`, `reopened_copy_exists()` and composite predicate `burndown_complete()`; unit tests AC#1–AC#4b plus production-path test driving `RunTestUseCase`.
- `crates/application/src/use_cases/run_test.rs` — production change: TEST promotes Fixed → Verified AND records "REGRESSION TEST" QA evidence via `state.add_evidence(...)` per CXA-F022 AC#2/#3 (the verification loop around lines 186–249).
- `crates/application/src/prompts.rs` — F001 surface under AC#5 guard: constants BASE / ENGINEERING_STANDARDS / role bodies plus the composer `system_prompt(role_section)`.
- `crates/application/src/prompts_resolve.rs` — role-body override seam (`compose_role_system`, `load_role_override`, `resolve_role_body`, `is_valid_role_key`); pure resolution through the files port (`WorkspaceFilesPort`) with KNOWN_ROLE_KEYS enforcing g2 rejection.
- `crates/application/tests/prompts_resolve.rs` — regression tests for contracts g1–g4 against pre-fix behaviour.
- `crates/domain/src/ticket.rs` — dependency links (`depends_on()`, `add_dependency`) and legal lifecycle transitions (`transition_to`) that gating status relies on.
- `crates/domain/src/kinds.rs` — Status enum (Open → InProgress → Fixed → Verified) and Role enum (DevBug, Test, Sa…).
- `crates/application/src/state/mod.rs` — evidence storage model consumed by the predicates: field ``ticket_evidence`` : BTreeMap\<String, Vec\<Evidence\>\> plus producer method ``add_evidence(ticket, kind, label, detail)``.

## Related

- **F001 prompt system** — the feature these four bugs gate; its surface invariants are pinned by AC#5 here. The override seam it builds on is described in [prompts_resolve] and [docs](editable_prompt_system.md).
- **CXA-F021 config-drift coverage gate** — a fellow Product-space burn-down-style ticket using the same evidence/provenance conventions (`wiki/Product/CXA-F021-config-drift-coverage-gate.md`).
- docs/CXA-F003.md — same doc convention; shows how a verified feature page is written.
