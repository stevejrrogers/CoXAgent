FOLDER: Configuration
# Config Struct and the missing "coverage" field build error (CXA-B004)

**Keywords:** Config, coxagent.json, struct initializer, missing field, coverage, CoverageConfig, build failure, config_load, lib.rs, EngineMapping

## Overview

CXA-B004 was filed as a build break: "Build fails: missing coverage field in Config struct initializer at lib.rs line 617." This page documents the project's top-level configuration type `Config` (persisted as `coxagent.json`) and what fixing that ticket changed. Anyone landing here from a "missing field in Config initializer" error — or looking to change which sections `coxagent.json` carries — will find where the type is defined, every place it is constructed by hand, and why adding or removing a section does or does not fail the build.

## How it works

The single source of truth for a project's runtime configuration is `Config`, defined in `crates/application/src/config.rs`. It is pure data (no IO); loading from disk is an adapter concern in `crates/app/src/config_load.rs`.

The type currently declares nine fields (all under `#[serde(default)]`, so an older or hand-written document that omits a section still loads):

```rust
pub struct Config {
    pub schema_version: u32,
    pub engine: EngineMapping,
    pub git: GitConfig,
    pub workflow: WorkflowConfig,
    pub architecture: Vec<crate::conformance::StackRule>,
    pub policy: PolicyConfig,
    pub deploy: DeployConfig,
    pub releases: ReleasesConfig,
    pub coverage: CoverageConfig,
}
```

CXA-B004 existed because Rust requires every non-spreaded struct literal to list **every** field. When `coverage` was added to the struct without updating all literal initializers at once, compilation failed with exactly this ticket's message at each missed site. The fix adds `coverage` (and its sibling `schema_version`) to every full literal:

1. The full-featured hub builds one literal inside `run_hub`, within its call to `build_engine(...)` at `crates/app/src/lib.rs:~629`. After the fix it lists all fields including:
   ```rust
   coverage: coxagent_application::config::CoverageConfig::default(),
   schema_version: coxagent_application::config::CONFIG_SCHEMA_VERSION,
   ```
2. Two other literals exist only in release-pipeline tests (`run_releases_tests.rs` and `run_releases_tdd_tests.rs` under `crates/application/src/use_cases/`). Each uses functional-update syntax (`..Config::default()`), so they survive any future field being added without edits.
3. Everything else reaches config through `load_config_with_probe(state_dir)` (`crates/app/src/config_load.rs`), which serde-deserializes into a private holding value containing both config and host-port probe. An absent section becomes its default; only a value that cannot be represented fails the load (see COX-B043).

At verification time (`cargo check -p coxagent-app`) against a tree carrying this fix the workspace compiles cleanly; no missing-field error reproduces.

## Usage

To reproduce whether any real missing-field error exists on your branch:

```
cargo check -p coxagent-app     # compile against your current tree
```

Confirm which fields are present in Config history:

```
git log --all -S "coverage:" --oneline -- crates/application/src/config.rs crates/app/src/lib.rs
```

Locate every hand-written initialization of each config section:

```
grep -rn ': Config {' crates/
grep -n 'fn run_hub' crates/app/src/lib.rs     # home of lib.rs's &Config { ... }
```

If you add or remove a section from Config , fix EVERY non-default-spreaded literal at once (`lib.rs` around line 629 plus any new ones); otherwise those sites fail with precisely this ticket's symptom.

## Interface

The relevant type for this ticket is `CoverageConfig`, defined in `crates/application/src/config.rs`:

```rust
pub struct CoverageConfig {
    pub enabled: bool,    // default true
    pub threshold: u32,   // default 3
}
```

It is carried as a single field of the top-level `crates/application/src/config.rs::Config`:

```rust
pub struct Config {
    pub schema_version: u32,
    pub engine: EngineMapping,
    pub git: GitConfig,
    pub workflow: WorkflowConfig,
    pub architecture: Vec<crate::conformance::StackRule>,
    pub policy: PolicyConfig,
    pub deploy: DeployConfig,
    pub releases: ReleasesConfig,
    pub coverage: CoverageConfig,
}
```

The loading adapter that turns a persisted document into a `Config` is:

```rust
// crates/app/src/config_load.rs
pub(crate) fn load_config_with_probe(state_dir: &Path) -> Result<LoadedConfig, String>;
pub(crate) struct LoadedConfig {
    pub(crate) config: Config,
    pub(crate) host_port_probe: Result<Option<u16>, ()>,
}
```

Each top-level section also carries its own nested struct (`EngineMapping`, `GitConfig`, `WorkflowConfig`, `PolicyConfig`, `DeployConfig`, `ReleasesConfig`) plus per-section defaults such as `default_coverage_enabled()` / `default_coverage_threshold()`.

## Configuration

There is no CLI flag or runtime environment setting for this ticket — CXA-B004 only concerns Rust source construction. Behavioural knobs that the `coverage` field carries live on `CoverageConfig`, whose defaults are set in `crates/application/src/config.rs`:

| Field | Default when omitted |
|---|---|
| `CoverageConfig::enabled` | `true` (`default_coverage_enabled`) |
| `CoverageConfig::threshold` | `3` (`default_coverage_threshold`) |

Every top-level section of `Config` is also under `#[serde(default)]`, so a document that omits a section (including one written before that field existed) still loads with that section's default rather than failing parse.

## Edge cases and limits

- This ticket was fixed by adding `coverage` + `schema_version` to every non-spreaded literal. It reproduces again only if someone adds another field without updating those literals.
- Line numbers drift constantly (the file was roughly 1697 lines at writing). Never treat lib.rs line number as stable truth; always recompile against your own tree.
- Only missing named fields break these literals. Adding fields under serde default changes deserialization but not construction correctness of existing spread-style initializers.
- Legacy or handwritten documents load by design: serde default makes an absent section distinct from corrupt data (see COX-B043).

## Code map

- `crates/application/src/config.rs` — defines `Config` (including the `coverage: CoverageConfig` field and `schema_version`) plus every nested section struct and its defaults.
- `crates/app/src/lib.rs` — hosts `run_hub`, which builds the sole full-featured `&Config { ... }` literal inside its `build_engine(...)` call (around line 629); this was the site that needed the added fields.
- `crates/app/src/config_load.rs` — loading adapter: `load_config_with_probe(&Path) -> Result<LoadedConfig, String>` reads `coxagent.json` and returns config + host-port probe.
- `crates/application/src/config_parse.rs` — parses persisted JSON into a schema-validated config (used by the loader).
- `crates/application/src/use_cases/run_releases_tests.rs`, `crates/application/src/use_cases/run_releases_tdd_tests.rs` — release-pipeline tests that build a partial `Config { releases: ..., ..Config::default() }`, so they need no change when fields are added.

## Related

- CXA-B043 / COX-B043 — "a malformed field must not wipe the whole config": absent section != corrupt document; governs load failure semantics in this loader.
- CXA-B042 — host-port expiry/healing logic in config_load used by run_loop.
- cxa-b001-docker-compose-deploy-failure.md (Engineering / Deployment) — deploy.host_port handling flows through the same load_config_with_probe adapter.
