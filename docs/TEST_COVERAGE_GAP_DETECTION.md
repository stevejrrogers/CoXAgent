FOLDER: -

# Test Coverage Gap Detection

**Keywords:** test coverage, gap detection, test_surface_block, TEST agent, RunTestUseCase, WorkspaceFilesPort, EXISTING TESTS, API SURFACE

## Overview

Test Coverage Gap Detection is how the TEST agent decides what to exercise without re-testing what already has coverage or guessing at endpoints. Before writing a single case it walks the working tree once — no LLM call — counts every test marker per file and collects every registered API route via `test_surface_block`, then feeds that inventory into the QA prompt so new bugs are found against real gaps rather than duplicated coverage or invented routes. It is for anyone running or extending the TEST role.

## How it works

The flow runs inside a single TEST pass (`RunTestUseCase::execute`, crates/application/src/use_cases/run_test.rs:61):

1. The use case loads project state and builds memory/shipped/knowledge blocks (run_test.rs:63-94).
2. It calls `prompts::test_surface_block(self.files.as_deref(), &self.work_dir)` (run_test.rs:97). With no attached files port this returns an empty string.
3. Inside `test_surface_block` a bounded iterative walk of `work_dir` uses only `WorkspaceFilesPort::list_dirs` / `list` / `read`. Directories whose names start with `.` or match `SKIP = ["target","node_modules",".git","dist","build","vendor",".venv"]` are pruned; recursion stops past 200 open stack entries and file reads are budgeted at 400 (prompts.rs:475-510).
4. Only source extensions count — `.rs`, `.ts`, `.tsx`, `.js`, `.go`, `.py`. For each read file it sums markers: occurrences of `#[test]`, `#[tokio::test]`, `def test_`, `func Test`, and `it(` (prompts.rs:527-531). Files with at least one marker become entries in the tests list; up to 24 route lines matching `.route(` or `@app.route` are captured (prompts.rs:535-546).
5. If nothing was found it returns an empty block; otherwise it renders two sections — “EXISTING TESTS” listing the top-8 most-covered files by marker count with totals, then “API SURFACE actually registered” up to 12 routes (prompts.rs:549-572), capped at 1600 chars.
6. The rendered block is appended to the TEST task prompt alongside shipped/memory/repo-map blocks (run_test.rs:99-110), so returned bugs get deduped against existing titles via parse_items → AddTicketUseCase (run_test.rs:121-153), and Fixed-not-resurfaced bugs advance toward Verified.

“Coverage” here means marker presence per file plus registered route lines — there is no line-level instrumentation anywhere; that boundary is deliberate.

## Usage

No manual steps required — detection happens automatically whenever TEST runs through its normal cycle wiring:

```text
TEST pass begins
  └─ RunTestUseCase::execute
       ├─ …memory/shipped/repo-map blocks…
       └─ test_surface_block(files_port?, work_dir)
            ├─ prune target/, node_modules/, .git/, dist/, build/, vendor/, .venv/
            ├─ sum #[test] / #[tokio::test] / def test_ / func Test / it( per source file
            ├─ collect ≤24 route lines (.route( / @app.route)
            └─ render "EXISTING TESTS …" + "API SURFACE …"   → capped at 1600 chars
```

A tester with genuine gap signal receives something like:

```text
EXISTING TESTS (12 across 5 files) — cover what these do NOT, and do not duplicate them:
- crates/app/tests/deploy_smoke.rs (6)
- crates/app/tests/hexagonal_gate.rs (4)

API SURFACE actually registered in the code — test these, not guesses:
- .route("/health", get(health))
```

If both lists come back empty within budget limits an empty string is appended instead of a misleading heading.

## Interface

Functions involved:

- `pub async fn test_surface_block(files: Option<&dyn WorkspaceFilesPort>, work_dir: &std::path::Path) -> String` — prompts.rs:470.
- Trait [`WorkspaceFilesPort`](crates/application/src/ports/outbound/workspace.rs): only four methods feed detection —
  - `async fn list_dirs(&self, dir) -> Vec<std::path::PathBuf>`
  - `async fn list(&self, dir) -> Vec<FileMeta>` (`FileMeta { path, modified_epoch, size }`)
  - `async fn read(&self, path) -> Option<String>`
- Infra adapter implementing that trait over real IO:
  - [`crates/infrastructure/src/workspace_files.rs`](crates/infrastructure/src/workspace_files.rs)
- Consumer driving it:
  - [`RunTestUseCase<S,E>`](crates/application/src/use_cases/mod.rs re-exports from run_test).

Constants governing detection behaviour inside prompts.rs:

| constant | value |
|----------|-------|
| SKIP | target/node_modules/.git/dist/build/vendor/.venv |
| stack bound | <200 open directories |
| file-read budget | ≤400 source reads per pass |
| route capture cap | ≤24 collected |
| rendered tests cap | top 8 files |
| rendered routes cap | top 12 |
| output char cap | ≤1600 |

Marker strings matched are fixed in code (`#[test]`, `#[tokio::test]`, etc.) — there is no configurable regex surface.

## Configuration

Test Coverage Gap Detection has **no dedicated configuration keys**. It responds only to:

- Presence of an attached files port on RunTestUseCase (`with_files(...)`); without one (`None`) detection returns an empty block silently.
- The workspace itself changes what is counted; all budget caps above can be changed only by editing their source constants.
- Engine selection comes from config via Role::Test resolution elsewhere in execute() but does not change how inventory is built.

Defaults therefore are the code constants listed under Interface above.

## Edge cases and limits

It deliberately does NOT provide:

- Line/branch instrumentation or percentage metrics — “coverage” means marker presence + route registration only.
- Guaranteed completeness on huge repos; a bounded walk truncates past ~200 stack entries or ~400 reads per pass.
- A correctness guarantee about overlapping markers across languages beyond heuristic counting totals.
- Any handling for engine failures beyond surfacing them as errors downstream in execute(); if both lists come back empty you get no section text rather than “no tests found”.

It fails gracefully throughout because all reads return Options via WorkspaceFilesPort rather than propagating fs errors into application logic.

## Code map

- `crates/application/src/prompts.rs:470` — `test_surface_block`: the bounded repo walk, marker counting, route capture, and block rendering. The heart of detection.
- `crates/application/src/use_cases/run_test.rs` — `RunTestUseCase`: calls `test_surface_block`, folds it into the TEST prompt, parses and dedupes returned bugs.
- `crates/application/src/use_cases/mod.rs` — module declaration and `pub use run_test::RunTestUseCase`.
- `crates/application/src/ports/outbound/workspace.rs` — trait `WorkspaceFilesPort` (list_dirs / list / read) that keeps fs out of application logic.
- `crates/infrastructure/src/workspace_files.rs` — real-filesystem adapter implementing `WorkspaceFilesPort`.

## Related

- Feature docs in this repo follow the same skeleton, e.g. docs/CXA-F003.md (optimistic concurrency for the state store); this page is a companion capability of the TEST role rather than a standalone service ticket.
- RunTestUseCase's verification half (regression promotion, evidence gate) lives in crates/app/tests (e.g. deploy_smoke.rs is a counted source itself).

