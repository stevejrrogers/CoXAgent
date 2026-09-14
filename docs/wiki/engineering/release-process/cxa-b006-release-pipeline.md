FOLDER: Release Process

# Release Pipeline (CXA-B006)

**Keywords:** release pipeline, milestone, git tag, release chore, REL ticket, RunReleasesUseCase, target_version, goal_complete, current_version

## Overview

The automated release pipeline tags a git version and files a `REL-*` Release chore whenever a product milestone's target version ships and its scope is done. It runs once per leader cycle inside `RunCycleUseCase::run_cycle`, so a release happens the moment a milestone is reached instead of waiting for a human or a daily slot. It is deliberately opt-in: creating tags mutates the managed codebase's git history, so nothing runs until `releases.enabled` is set.

CXA-B006 ("Build fails: unclosed impl delimiter in run_release_pipeline.rs") landed this feature. The ticket title names `run_release_pipeline.rs`, an alternate scratch filename used during development; that exact path never exists in any commit — the build error was an unclosed `impl` block delimiter caught before landing. The committed home file is `crates/application/src/use_cases/run_releases.rs`. Search by symbol `RunReleasesUseCase`, not by filename.

## How it works

Milestones are authored by `RunMilestonesUseCase` (the PO) into `state.milestones` as ``Milestone { name, goal, target_version: "MAJOR.MINOR.PATCH", goal_complete=false, fulfilled=false }``. Each carries an explicit semver target version.

Each leader cycle calls `self.releases().execute()` (`RunReleasesUseCase::execute`) after PO milestones planning; the builder lives at `crates/application/src/use_cases/cycle/wiring.rs` in `.releases()`. In order:

1. **Opt-in gate** — returns empty unless `config.releases.enabled`.
2. **Git present gate** — returns empty if no GitPort backend was attached via `.with_git(...)`.
3. Iterate a clone of state's milestones; skip any already passing these gates:
   - **Already released** — skip if `fulfilled == true`.
   - **Gate 1 / Scope ready** — skip unless explicitly set by PO/human; never derived from code alone (`goal_complete`).
   - **Gate 2 / Version reached** — skip if version parse fails or current version is still below target (`SemVer::parse(&m.target_version)` vs `state.current_version`).
   - **Gate 3 / Tag exists** — skip + log activity if ``git.tag_exists(work_dir, &m.name)`` (idempotent across retries and out-of-band tags).
4. For each passing milestone:
   - Create an annotated tag on HEAD via ``git.create_tag(work_dir, &m.name /*milestone name*/, "HEAD", author)`` under the identity from `.release_author()` (configured commit email or bot fallback).
   - Build release notes from deploy history spanning last-shipped version (highest deploy below target) up to current.
   - Create a Chore ticket id ``REL-<name>`` via domain ``Ticket::new`` (`TicketType::Chore`, Priority Medium), adding each producing deploy ticket as a formal dependency (`add_dependency(Role::System, id)`).
   - Post notes to the team room with `/comment`, mark the milestone fulfilled.
5. Always persist state at the end (even when nothing new released) so skip/tag-exists audit entries survive.

Returns empty when disabled/gated; otherwise the list of newly-released milestone names.

## Usage

Enable automatic releases:

```json
{
  "releases": { "enabled": true }
}
```

The PO lays out milestones through normal operation (or by hand editing state). A run produces:

```
RELEASE /comment
Release 0_5-stability (v0.5.0).
CXA-123 Fix auth session expiry — v0.5.0
CXA-141 Persist dashboard filters — v0.5.0
```

and creates ticket:

```
REL-0_5-stability | Chore | Medium | depends_on [CXA-123 CXA-141]
Release 0_5-stability
```

Unit tests run off fake store/git ports:
- `crates/application/src/use_cases/run_releases_tests.rs`
- `crates/application/src/use_cases/run_releases_tdd_tests.rs`

## Interface

Public type + builder methods on generic-over-port use case:

| Member | Signature |
|--------|-----------|
| struct | ``pub struct RunReleasesUseCase<S: StateStorePort>`` |
| constructor | `<S>::new(store: Arc<S>, work_dir: PathBuf) -> Self` |
| builder | `.with_git(git: Option<Arc<dyn GitPort>>) -> Self` |
| builder | `.with_config(config: Config) -> Self` |
| entrypoint | `.execute(&self) -> Result<Vec<String>, AppError>` |

Exported at crate-root facade (`crates/application/src/use_cases/mod.rs`: ``pub use run_releases::RunReleasesUseCase;``).

IO goes through ports only; application code never does direct fs/process work:
```rust
// GitPort outbound port signatures exercised (impls at system_git)
async fn create_tag(
    work_dir    : &Path,
    name        : &str,
    ref_target  : &str /* pass "HEAD" here */,
    author      : &GitAuthor)
  -> Result<(), PortError>;

async fn tag_exists(
    work_dir : &Path,
    name     : &str /* resolves refs/tags/{name} */)
  -> bool;
```

Persisted/data types:
```rust
// crates/application/src/config.rs:632 ReleasesConfig { enabled } // single field
// crates/application/src/state/work.rs:171 Milestone {
name           : String,
goal           : String,
target_version : String,
goal_complete  : bool /* serde default false */ ,
fulfilled      : bool /* serde default false */ ,
}
```

## Configuration

All settings live under top-level config sections persisted as coxagent.json; every field serde-defaulted so omission loads defaults instead of failing load.

Only one setting changes this feature's behaviour:

```jsonc
"releases": { "enabled": false } // master switch; off → tags/chores never created/filed
```

Also referenced internally but unchanged here:

```jsonc
"git.commit_email": "" // tag-author identity fallback → coxagent-bot@users.noreply.github.com when empty
```
No further configuration keys drive this use case.

## Edge cases and limits

The pipeline fails safe rather than erroring on ordinary conditions; all of these are silent skips that log an activity entry where noted:

- **Disabled or no git** — returns empty without touching state if disabled or no GitPort wired; no tag, no chore, no error.
- **Already released** — skipped + logged when fulfilled or its git tag already exists (idempotent across restarts and retries).
- **Scope not ready** — blocked even at-or-past target until explicitly marked shippable.
- **Version not reached / unparseable** — skipped until current version catches up on a later cycle.
- **Tag creation failure** — surfaces as an ``AppError`` wrapped in ``PortError::Backend("create tag for '<name>': …")``; other milestones still persist because state saves unconditionally afterward.

Deliberately out of scope: no push to remotes, changelog-file generation, or artifacts beyond annotated tag + chore ticket; rollback of bad releases belongs to deploy concerns elsewhere.

## Code map

Application layer (the feature):
- `crates/application/src/use_cases/run_releases.rs` — the whole pipeline: opt-in/git gates, milestone iteration, annotated tag creation, release-note assembly from deploy history, REL chore ticket + dependency wiring.

Wiring and facade:
- `crates/application/src/use_cases/mod.rs` — crate-root re-export of `RunReleasesUseCase`.
- `crates/application/src/use_cases/cycle/wiring.rs` — the `.releases()` builder attaching config + git ports to the runner.
- Invoked from the leader phase of `RunCycleUseCase::run_cycle` (see `crates/application/src/use_cases/cycle/mod.rs`).

Ports and state:
- `crates/infrastructure/src/git/system_git.rs:330` (`create_tag`) and :354 (`tag_exists`) — concrete GitPort adapters; round-trip test at :479.
- `crates/application/src/config.rs:632` — `ReleasesConfig { enabled }`, serde-defaulted off.
- `crates/application/src/config.rs:704` — top-level `releases: ReleasesConfig` field on `Config`.
- `crates/application/src/state/work.rs:171` — persisted data type Milestone.

Tests:
- `crates/infrastructure/src/git/system_git.rs:479` — create/tag-exists round-trip test.

