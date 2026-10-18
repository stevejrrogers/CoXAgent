FOLDER: Configuration
# Config struct and the missing "coverage" field build error (CXA-B004)

**Keywords:** Config, coxagent.json, struct initializer, missing field, coverage, CoverageConfig, CONFIG_SCHEMA_VERSION, config_drift_gate, build failure

## Overview

CXA-B004 was filed as a build break: "Build fails: missing `coverage` field in Config struct initializer at lib.rs line 617." The root cause is mechanical — Rust requires every non-spreaded struct literal to list **every** field of that type. When a new top-level section (`coverage`) was added to `Config`, any inline `Config { ... }` literal that did not also add that field failed to compile with exactly this ticket's message. This page documents where `Config` lives, why adding or removing a section does or does not break the build, and where every literal initializer sits today. It is for anyone hitting a "missing field in Config initializer" error or changing which sections `coxagent.json` carries.

## How it works

The single source of truth for a project's runtime configuration is `Config`, defined in `crates/application/src/config.rs`. It is pure data (no IO); reading it from disk is an adapter concern in `crates/app/src/config_load.rs`.

The type currently declares **eight** sections (all under `#[serde(default)]`, so an older or hand-written document that omits a section still loads with defaults rather than failing):

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub engine: EngineMapping,
    pub git: GitConfig,
    pub workflow: WorkflowConfig,
    pub architecture: Vec<crate::conformance::StackRule>,
    pub policy: PolicyConfig,
    pub deploy: DeployConfig,
    pub releases: ReleasesConfig,
    #[serde(default)]
    pub coverage: CoverageConfig,
}
```

(It originally held seven sections; CXA-F021 added the eighth, landing as fix commit 3f8d8ec which resolved CXA-B004 on HEAD.) Because every section has both a serde default and most carry an explicit container-level default (`impl Default`), an absent section deserializes into its documented default instead of failing — but that only helps code paths that reach config through serde. A hand-written inline literal has no such protection.

CXA-B004 existed because Rust construction rules are stricter than serde deserialization:

1. **Serde path (safe):** most callers reach config through load helpers (`load_config_with_probe(state_dir)` in `config_load.rs`) which deserialize text via serde into each section's default when omitted.
2. **Literal path (dangerous):** any inline `&Config { ... }` written by hand must enumerate all eight fields or use functional-update spread (`..Default::default()`). Adding a ninth section without updating these literals reproduces CXA-B004 verbatim.

Every current hand-written location of the top-level type:

- **The hub analyzer engine** builds one full-featured literal inside its call to `build_engine(...)` within run_hub at lines 629–647 of crates/app/src/lib.rs . After the fix this lists all eight fields including:
  ```rust
  coverage: coxagent_application::config::CoverageConfig::default(),
  ```
- **Two release-pipeline tests**, run_releases_tests.rs and run_releases_tdd_tests.rs under crates/application/src/use_cases/, each construct their fixture with functional-update syntax:
  ```rust
  .with_config(Config {
      releases: ReleasesConfig { enabled: true },
      ..Config::default()
  })
  ```
  Because they spread defaults they survive any future top-level field being added without edits.
- Everything else reaches config through load_config_with_probe → parse_config , never through a manual literal.

Parsing / schema anchoring happens in parse_config within crates/application/src/config_parse.rs . It first deserializes text into an untyped map so it can inspect the persisted header before committing to typed defaults; if a document carries a top-level header key schema_version whose value exceeds const CONFIG_SCHEMA_VERSION (=1) , load returns Err naming field "schema_version" rather than accepting future-shaped data blind or defaulting it away (the same fail-closed posture state.json already has via json_store.parse_checked). Documents that omit schema_version predate the anchor and load as prior-version state whose user-set values are preserved.

At verification time both crates compile cleanly against this branch:

```
cargo check -p coxagent-application   # application crate (config types)
cargo check -p coxagent-app           # binary crate containing lib.rs initializer
```

No missing-field error reproduces on either.

## Usage

To reproduce whether any real missing-field error exists on your tree:

```
cargo check -p coxagent-app     # compile against your current tree
```

Confirm which top-level sections exist on Config:

```
grep -n 'pub [a-z]*:' crates/application/src/config.rs | grep -v fn | head -30
```

Locate every hand-written initialization of each config section:

```
grep -rn '&*[[:space:]]*[Cc]onfig {' crates/
grep -n 'fn run_hub' crates/app/src/lib.rs     # home of lib.rs &Config { ... }
```

If you add or remove a top-level section from Config , fix EVERY non-default-spreaded literal at once — lib.rs around line 629 plus any new ones you introduce — otherwise those sites fail with precisely this ticket's symptom ("missing field `<name>` in initializer").

## Interface

The relevant type introduced alongside this fix is CoverageConfig , defined beside Config in crates/application/src/config.rs :

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageConfig {
    #[serde(default = "default_coverage_enabled")]
    pub enabled: bool,
    #[serde(default = "default_coverage_threshold")]
    pub threshold: u32,
}
```

Defaults come from named container-default functions rather than Rust's derived zero-value so an unset knob stays documented-and-true instead of silently becoming false / 0 :

| Field | Default when omitted |
|---|---|
| CoverageConfig.enabled | true (default_coverage_enabled) |
| CoverageConfig.threshold | 3 (default_coverage_threshold) |

Its own impl Default calls those same two functions so both serde omission and direct construction agree on {true ,3}. There is no CLI flag or runtime environment variable behind this ticket; load-failure semantics follow COX-B043 and are enforced by parse_config .

Schema anchor constant exported from the config module:

```rust
pub const CONFIG_SCHEMA_VERSION: u32 = 1;
```

parse_config reads it against the optional persisted header key schema_version ; there is no helper serializing that key back onto disk today — earlier notes referencing such an accessor are inaccurate. Versioning deliberately keeps state-style headers separate from typed fields so drift between writer and reader fails loudly at load rather than being silently re-defaulted.

## Configuration

This ticket introduces no behaviour switch beyond what coxagent.json already governs. What changes behaviour here is purely structural:

- Adding another top-level field to Config changes nothing at runtime but forces every non-spreaded literal to be updated or CI breaks.
- The presence vs absence of schema_version on disk changes whether parse_config refuses (= greater-than-supported) or accepts-and-migrates (= equal-to-or-absent).
- Defaults for omitted coverage knobs come from default_coverage_enabled() -> true and default_coverage_threshold() -> 3 ; there are no other knobs affected by CXA-B004/CXA-F021.

An example minimal coverage section inside coxagent.json:

```json
{
  "coverage": { "enabled": true, "threshold": 3 }
}
```

Because none appear as env vars or CLI flags by design — per-project settings belong in coxagent.json under governance-policy discipline — there is nothing further to configure here beyond editing that file.

## Edge cases and limits

- This ticket was fixed by adding coverage everywhere needed; it reproduces again only if someone adds another independent top-level section without updating those literals.
- Only full non-spreaded literals break under Rust construction rules when fields change; spread-style (`..Default::default()`) literals never do.
- Legacy/handwritten documents load by design because every section uses #[serde(default)] : an absent section differs from corrupt data.
- A document written by a newer build carrying schema_version > CONFIG_SCHEMA_VERSION refuses to load outright — never accepted blind nor silently re-defaulted into today's view (config_drift_gate AC3).
- Line numbers drift constantly (~1710 lines total depending which side stated them). Never treat lib.rs line numbers as stable truth; always recompile against your own tree.
- There is currently NO runtime consumer wiring CoverageConfig into gap detection yet — lib.rs only pins it to defaults; acceptance tests exercise round-trip/defaults/schema-drift/migration (see Code map). The pass logic itself lands separately under docs/TEST_COVERAGE_GAP_DETECTION.md .

## Code map

- crates/application/src/config.rs — domain: declares `Config` (all eight sections) and `CoverageConfig`, plus named default functions `default_coverage_enabled()` / `default_coverage_threshold()`, explicit `impl Default for CoverageConfig`, and const `CONFIG_SCHEMA_VERSION = 1`.
- crates/application/src/config_parse.rs — parsing: `parse_config(text)` deserializes to untyped JSON first, enforces schema_version <= CONFIG_SCHEMA_VERSION via header-key check (refuses newer documents naming field "schema_version"), returns typed Config or ConfigParseError.
- crates/app/src/config_load.rs — adapter: load_config(state_dir) and load_config_with_probe(state_dir) read coxagent.json from disk, call parse_config, apply host-port healing; absent file -> Config::default(); corrupt/unrepresentable -> Err (fail closed).
- crates/app/src/lib.rs — presentation wiring: run_hub builds its analyzer engine from an inline full-featured &Config { ... } literal at lines 629–647; after CXA-F021 this includes coverage: CoverageConfig::default() . The single non-spreaded production literal.
- crates/application/src/lib.rs — re-exports config surface publicly: Config, CoverageConfig, CONFIG_SCHEMA_VERSION, parse_config, etc., so other crates reach these by path.
- crates/application/src/use_cases/run_releases_tests.rs and run_releases_tdd_tests.rs — release-pipeline tests construct Config fixtures with functional-update spread (..Config::default()), which survive any added top-level field without edits.
- crates/app/tests/config_drift_gate.rs — TDD acceptance gate for CXA-F021 covering round-trip on disk, omitted-field defaults, newer-schema refusal at load, hot-reload threshold propagation via re-read, and prior-version migration preserving user-set coverage values.

## Related

- wiki/product/cxa-f021.md (docs/wiki .product copy) — CXA-F021 "Coverage field + Config-drift compile gate", the ticket that actually landed CoverageConfig + CONFIG_SCHEMA_VERSION + drift gate which resolves CXA-B004's build break.
- docs/CXA-B004-config-initializer.md / docs/TEST_COVERAGE_GAP_DETECTION.md — notes on where lib.rs initializers sit today (B004) and where coverage wiring eventually flows into gap detection.
- COX-B043 semantics documented throughout config loading pages — fail-closed posture for unrepresentable documents versus defaulting an absent section.



