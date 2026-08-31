FOLDER: deployment

# Release Pipeline Wiring & Re-export Guard

**Keywords:** RunReleasesUseCase, run_releases, release pipeline, milestone tag, REL ticket, re-export guard, mod_guard_tests, mod.rs facade, cycle wiring

## Overview

CXA-F027 completed the wiring that lets the automated release pipeline actually run from the agent cycle and added a test guard that keeps that wiring intact. This page shows where `RunReleasesUseCase` is built and invoked end-to-end (crate facade → cycle builder → leader loop) and why a half-wired use case now fails loudly in CI instead of silently at every call site. It is for anyone changing release behaviour or touching `crates/application/src/use_cases/mod.rs`.

## How it works

The milestone-triggered release pipeline lives entirely in one application-layer use case, `RunReleasesUseCase<S: StateStorePort>`, whose `execute()` iterates persisted milestones and releases any that have reached their target version with a complete goal. Two pieces of "wiring" make it reachable:

1. **Crate-root facade** — `crates/application/src/use_cases/mod.rs` declares `pub mod run_releases;` (line 26) and re-exports it as `pub use run_releases::RunReleasesUseCase;` (line 56). Downstream code constructs it through `crate::use_cases::RunReleasesUseCase`, never through an internal module path.
2. **Cycle builder** — `cycle/wiring.rs:44` (`RunCycleUseCase::releases()`) builds one instance per cycle via `<S>::new(store, work_dir).with_config(config).with_git(git)`, handing it the same store / work-dir / config / git ports as every other agent sub-use-case.

Inside a leader tick of `RunCycleUseCase::run_cycle` (`cycle/mod.rs:846`) the result is awaited:

```rust
match self.releases().execute().await {
    Ok(released) if !released.is_empty() =>
        self.report("RELEASE", &format!("released milestone(s): {}", released.join(", "))),
    Ok(_) => {}
    Err(e) => report.errors.push(format!("RELEASE: {e}")),
}
```

It runs each leader cycle (right after PO milestones planning at line 837), not on a daily slot — so a milestone ships the moment its version + goal gates pass.

The **guard** (`mod_guard_tests.rs`) scans the single canonical manifest for this tree — pulled in via ``include_str!("mod.rs")`` so it is hermetic to CWD drift — with three checks:
- every top-level `pub mod <name>;` declared in mod.rs must appear as a module qualifier (`run_releases::`) inside some `pub use ...;`, or be an allow-listed internal helper (`INTERNAL_HELPERS = ["ceremony", "approval_memory", "approval_risk"]`);
- sanity-guards its own scanner so it cannot silently stop working (≥20 declarations found; must include `run_releases`);
- asserts specifically that ``RunReleasesUseCase`` stays re-exported from this facade.

A module declared but never re-exported previously failed far away at each call site; now it fails here with ``declared but not facade-re-exported (and not an internal helper): <name>``.

## Usage

There is no HTTP endpoint or CLI flag for this feature; you exercise it by running the agent until a milestone ships. To drive it directly, enable auto-releasing and let PO planning lay out milestones:

```json
{
  "releases": { "enabled": true }
}
```

With current version ≥ target_version and goal_complete set on a milestone, one leader cycle emits an activity entry plus a RELEASE room comment:

```
RELEASE /comment
Release 0_5-stability (v0.5.0).
CXA-123 Fix auth session expiry — v0.5.0
CXA-141 Persist dashboard filters — v0.5.0
```

and files a chore ticket referencing each producing deploy ticket as a formal dependency:

```
REL-0_5-stability | Chore | Medium | depends_on [CXA-123 CXA-141]
Release 0_5-stability
```

To verify wiring integrity locally (the guard's own tests):

```bash
cargo test -p coxagent --lib use_cases::mod_guard_tests
```

## Interface

Public type + builder methods on generic-over-port use case:

| Member | Signature |
|--------|-----------|
| struct | ``pub struct RunReleasesUseCase<S: StateStorePort>`` |
| constructor | `<S>::new(store: Arc<S>, work_dir: PathBuf) -> Self` |
| builder | `.with_git(git: Option<Arc<dyn GitPort>>) -> Self` |
| builder | `.with_config(config: Config) -> Self` |
| entrypoint | `.execute(&self) -> Result<Vec<String>, AppError>` |

Facade + builder surface:
```rust
// crates/application/src/use_cases/mod.rs:56
pub use run_releases::RunReleasesUseCase;

// crates/application/src/use_cases/cycle/wiring.rs:44 — inside impl RunCycleUseCase<S,E>
pub(super) fn releases(&self) -> crate::use_cases::RunReleasesUseCase<S>;
```

GitPort outbound port signatures exercised by this use case (concrete impls at system_git):
```rust
async fn create_tag(&self,
    work_dir   : &Path,
    name       : &str,
    ref_target : &str /* pass "HEAD" here */,
    author     : &GitAuthor)
  -> Result<(), PortError>;

async fn tag_exists(&self,
    work_dir : &Path,
    name     : &str /* resolves refs/tags/{name} */)
  -> bool;
```

Persisted/data types:
```rust
// crates/application/src/state/work.rs — Milestone {
name           : String,
goal           : String,
target_version : String,
goal_complete  : bool /* serde default false */,
fulfilled      : bool /* serde default false */,
}

// crates/application/src/config.rs ReleasesConfig { enabled } // see Configuration below
```

Guard internals worth knowing if you change mod.rs:
```rust
// crates/application/src/use_cases/mod_guard_tests.rs
const INTERNAL_HELPERS   = ["ceremony", "approval_memory", "approval_risk"];
fn parse_declarations(src)-> Vec<String>          // pub mod <name>;
fn facade_path_prefixes(src)-> HashSet<String>   // <ident>:: inside pub use lines
fn missing_from_facade(...)-> Vec<String>
#[test] fn every_declared_module_is_facade_exposed_or_internal_helper()
```

## Configuration

All settings live under top-level config sections persisted as coxagent.json; fields are serde-defaulted so omission loads defaults rather than failing load.

Only one setting drives this specific milestone-triggered pipeline:

```jsonc
"releases": { "enabled": false } // master switch; off → tags/chores never created/filed
```

Referenced internally for tag-author identity fallback:

```jsonc
"git.commit_email": "" // empty → coxagent-bot@users.noreply.github.com used to create tags inline at tag time (no ambient git config needed)
```

Related but NOT consumed by this use case:

```jsonc
"releases.cut_every_days": 0 // cadence cut feature lives in cycle/release_cut.rs (conventional commits + release PR), not here; see Related.
```

## Edge cases and limits

The pipeline fails safe rather than erroring on ordinary conditions; all of these are silent skips except where noted:

- **Disabled or no git wired** — returns empty immediately without touching state when `enabled=false` or no GitPort attached via `.with_git(...)`.
- **Already released / tagged** — skipped when `fulfilled==true`, or when git already has a tag for that milestone name (`tag_exists`) with an activity entry logged.
- **Scope not ready** — blocked even at/past target until explicitly marked shippable (`goal_complete == false`); never derived from code alone.
- **Version unparseable / not reached** — skipped until current version catches up on a later cycle.
- **Tag creation failure** — surfaces as ``AppError`` wrapping ``PortError::Backend("create tag for '<name>': …")``; other milestones still persist because state saves unconditionally afterward.
- The chore id takes the form ``REL-[milestone-name]`` literally from the milestone's name field; names containing characters invalid for git refs may fail at tag creation rather than being sanitised.

Deliberately out of scope here:
- No push to remotes after tagging.
- No changelog-file generation beyond prose notes assembled from deploy history.
This page documents CXA-F027's wiring + guard deliverable and how they integrate existing behaviour described more fully by CXA-B006 ("Release Pipeline").

## Code map

Application layer (the feature):
- `crates/application/src/use_cases/run_releases.rs` — `RunReleasesUseCase`: opt-in/git gates, milestone iteration, annotated tag creation, release-note assembly from deploy history, REL chore ticket + dependency wiring.

Wiring + facade (what CXA-F027 added):
- `crates/application/src/use_cases/mod.rs:26` — declares `pub mod run_releases;`.
- `crates/application/src/use_cases/mod.rs:56` — crate-root re-export `pub use run_releases::RunReleasesUseCase;`.
- `crates/application/src/use_cases/mod_guard_tests.rs` — the re-export wiring guard (scans mod.rs via include_str!, allow-list of internal helpers, parser regression tests).
- `crates/application/src/use_cases/mod.rs:63` — declares `mod mod_guard_tests;` under `#[cfg(test)]`, so the guard ships with the crate's tests.
- `crates/application/src/use_cases/cycle/wiring.rs:44` — `.releases()` builder attaching config + git ports to a fresh instance.
- Invoked from the leader phase of `RunCycleUseCase::run_cycle` at `crates/application/src/use_cases/cycle/mod.rs:846`.

Ports and state:
- `crates/infrastructure/src/git/system_git.rs` (`create_tag`, `tag_exists`) — concrete GitPort adapters.
- `crates/infrastructure/src/git/system_git.rs:~479` — tag create/exists round-trip test.
- `crates/application/src/config.rs:722` — ``ReleasesConfig { enabled, cut_every_days }``; see Configuration for which fields this use case reads.
- `crates/infrastructure` / domain types (`SemVer`, `Ticket`, etc.) referenced via coxagent-domain in run_releases.rs.

Tests:
- `crates/application/src/use_cases/mod_guard_tests.rs`
- declared alongside at mod.rs:65–68: ``run_releases_tdd_tests`` and ``run_releases_tests``.

## Related

- **Release Pipeline (CXA-B006)** — `docs/CXA-B006-release-pipeline.md` documents the milestone-triggered pipeline this ticket wired up and guarded; CXA-F027's deliverable is the missing wiring + guard on top of that feature.
- **Cadence release cuts** — `crates/application/src/use_cases/cycle/release_cut.rs`, driven by `releases.cut_every_days`: a separate conventional-commit bump + release-PR path that shares `ReleasesConfig` but is not part of `RunReleasesUseCase`.
- **Milestone authorship** — `RunMilestonesUseCase` (the PO) produces what this pipeline consumes; runs each cycle before releases.
- **mod.rs facade family** — other crate-root re-exports in `crates/application/src/use_cases/mod.rs` follow the same pattern; the guard now protects all of them, not just releases.
- Ticket follow-ups: CXA-B052 / CXA-B055 tracked getting this guard merged into build/main.
