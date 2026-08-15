FOLDER: Configuration
# Config Struct and the missing "coverage" field build error (CXA-B004)

**Keywords:** Config, coxagent.json, struct initializer, missing field, coverage, CoverageConfig, build failure, config_load

## Overview

CXA-B004 was filed as a build break: "Build fails: missing coverage field in Config struct initializer at lib.rs line 617." This page documents the project's top-level configuration type named Config (persisted as `coxagent.json`) and what fixing that ticket changed. Anyone landing here from a "missing field in Config initializer" error - or looking to change which sections `coxagent.json` carries - will find where the type is defined and why adding or removing a section does or does not fail the build.

## How it works

The single source of truth for a project's runtime configuration is `Config`, defined in `crates/application/src/config.rs`. It is pure data (no IO); loading from disk is an adapter concern in `crates/app/src/config_load.rs`.

The type currently declares eight fields (all under `#[serde(default)]`, so an older or hand-written document that omits a section still loads):

```rust
pub struct Config {
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

CXA-B004 existed because Rust requires every non-spreaded struct literal to list **every** field. When `coverage` was added to the struct without updating all literal initializers at once, compilation failed with exactly this ticket's message at each missed site.

Note on history labels (`git log --all -S "coverage:" -- crates/application/src/config.rs crates/app/src/lib.rs`): whether that search turns anything up depends on your branch's history. The commits literally tagged CXA-B004 carry mostly unrelated content - an autoheal watchdog script plus config-load and PR-review-gate test changes - so their labels can be misleading; verify against your own tree rather than trusting commit names alone.

The fix adds `coverage` to every full literal:

1. The full-featured hub builds one literal inside its call to `build_engine(...)` within run_hub at about line 629 of crates/app/src/lib.rs . After the fix it lists all fields including:
   ```rust
   coverage: coxagent_application::config::CoverageConfig::default(),
   ```
2. Two other literals exist only in release-pipeline tests (`run_releases_tests.rs` and `run_releases_tdd_tests.rs` under crates/application/src/use_cases/). Each uses functional-update syntax (`..Config::default()`), so they survive any future field being added without edits.
3. Everything else reaches config through load_config_with_probe(state_dir) (`crates/app/src/config_load.rs`), which serde-deserializes into LoadedConfig holding both config and host-port probe; schema anchoring / parsing happens in parse_config within crates/application/src/config_parse.rs . An absent section becomes its default; only a value that cannot be represented fails load semantics like COX-B043 describes.

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
grep -n 'fn run_hub' crates/app/src/lib.rs     # home of lib.rs &Config { ... }
```

If you add or remove a section from Config , fix EVERY non-default-spreaded literal at once (`lib.rs` around line 629 plus any new ones); otherwise those sites fail with precisely this ticket's symptom.

## Interface

The relevant type for this ticket is CoverageConfig , defined beside Config in crates/application/src/config.rs :

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageConfig {
    #[serde(default = "default_coverage_enabled")]
    pub enabled: bool,
    #[serde(default = "default_coverage_threshold")]
    pub threshold: u32,
}
```

Defaults come from named container defaults rather than Rust zero-value so an unset knob stays documented-and-true instead of silently becoming false / 0:

| Field | Default when omitted |
|---|---|
| CoverageConfig.enabled | true (default_coverage_enabled) |
| CoverageConfig.threshold | 3 (default_coverage_threshold) |

It is carried as one field of the top-level Configuration struct above along with EngineMapping / GitConfig / WorkflowConfig / PolicyConfig / DeployConfig / ReleasesConfig . There is no CLI flag or runtime environment setting for this ticket; behavioural knobs surrounding load failure semantics live elsewhere per COX-B043 notes about fail-closed parse rather than silent defaults.

## Edge cases and limits

- This ticket was fixed by adding `coverage` and fixing every non-spreaded literal that lacked it; it reproduces again only if someone adds another independent top-level section without updating those literals.
- Only missing named fields break these literals under Rust construction rules unless they use spread (`..Config::default()`); adding serde-defaulted fields changes deserialization but not construction correctness of existing spread-style initializers.
- Legacy or handwritten documents load by design: an absent section is distinct from corrupt data via `#[serde(default)]`, referenced above as the COX-B043 semantics.
- Line numbers drift constantly (~1695 vs ~1697 lines depending which side stated them). Never treat lib.rs line number as stable truth; always recompile against your own tree.
