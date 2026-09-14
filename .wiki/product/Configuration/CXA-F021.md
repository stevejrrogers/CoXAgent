FOLDER: Configuration
# CXA-F021 - Coverage field + Config-drift compile gate

**Keywords:** coverage; CoverageConfig; threshold; schema_version; CONFIG_SCHEMA_VERSION; config drift; config_parse; coxagent.json

## Overview

Adds the missing `coverage` section to the top-level project Config and a fail-closed Config-drift gate at load time. Before this ticket there was no typed coverage surface on `Config`; after it there is a round-trippable `CoverageConfig` with explicit defaults (`enabled=true`, `threshold=3`) plus refusal of any persisted document written by a newer build. For operators setting per-project quality gates and for agents that read/write `coxagent.json`.

## How it works

Types live in `crates/application/src/config.rs`; the load lives in `config_parse::parse_config`.

- **Type surface.** New struct `CoverageConfig` carries `.enabled: bool` and `.threshold: u32`. Each field uses an explicit container-default function (`default_coverage_enabled() -> true`, `default_coverage_threshold() -> 3`) rather than Rust's derived zero-value (COX-B043). Its own `Default` impl calls those functions.
- **Top-level wiring.** `Config::coverage` is annotated `#[serde(default)]`, like every sibling section (`engine`, `git`, `policy`, ...). An omitted section loads with defaults instead of failing the document.
- **Schema anchor.** Const `CONFIG_SCHEMA_VERSION = 1`. Persisted under key `schema_version`, defaulted via `current_schema_version()`.
- **Drift gate.** In `parse_config`, if a deserialized doc's `schema_version > CONFIG_SCHEMA_VERSION`, load returns an error naming field `schema_version`. A future-written doc is refused outright - never accepted blind and never defaulted away. Older or absent versions pass through as migration inputs.
- **Consumer wiring.** The hub builds its analyzer engine from an explicit full Config literal at crates/app/src/lib.rs (line ~629), setting coverage to default and pinning schema version.

Hot reload holds structurally: each cycle re-reads coxagent.json on disk (the same path used for engine changes), so saved values are seen next read without restart.

## Usage

Acceptance criteria map one-to-one to tests in crates/app/tests/config_drift_gate.rs:

AC1 round-trip        -> coverage_section_round_trips_exactly_on_disk
AC2 omitted-field def -> omitted_coverage_applies_the_documented_defaults
AC3 drift gate        -> newer_persisted_schema_is_refused_at_load_not_defaulted
AC4 hot reload        -> threshold_change_reaches_the_next_pass_without_a_restart
AC5 bump migration    -> prior_version_state_migrates_and_preserves_user_coverage

Example document:

{ "schema_version": 1, "coverage": { "enabled": false, "threshold": 7 } }

Round-trip:
let cfg = parse(r#"{ "coverage": { "enabled": false, "threshold": 7 } }"#);
assert_eq!(cfg.coverage.enabled, false);
assert_eq!(cfg.coverage.threshold, 7);

Omitted section applies documented defaults (true / 3):
let cfg = parse(r#"{ }"#);
assert_eq!(cfg.coverage.enabled, true);
assert_eq!(cfg.coverage.threshold, 3);

Drift gate refuses future schema:
let outcome = parse_config(&format!(r#"{{"schema_version":{}}}"#, CONFIG_SCHEMA_VERSION + 1));
assert!(outcome.is_err());

Verify locally:
cd crates/app && cargo test --test config_drift_gate   # all five green

## Interface

- Struct CoverageConfig fields .enabled/.threshold (serde round-trip equality).
- Field Config::coverage (#[serde(default)]); JSON key literally 'coverage'.
- Const CONFIG_SCHEMA_VERSION = 1.
- JSON anchor key literally 'schema_version'.
- fn config_parse::parse_config(text) -> Result<Config, ConfigParseError> carrying .field (.dotted path or <document>) and .detail.
- Helper fns default_coverage_enabled()/default_coverage_threshold().

## Configuration

Two knobs under serialized object keyed 'coverage':
| Knob | Type | Serialized key | Default |
| Master switch for gap detection | bool | coverage.enabled | true |
| Threshold % below which gap filed | u32 | coverage.threshold | 3 |

Both produced by container-default fns rather than derived zeros (COX-B043).

## Edge cases and limits

Deliberately does NOT do runtime gap detection yet - this ticket lands only the config surface plus the drift gate. A threshold change reaches the next pass only because each cycle re-reads coxagent.json (no dedicated watcher). A future schema_version is refused hard rather than migrated automatically here; older docs are accepted as-is without rewriting them.

## Code map

crates/application/src/config.rs -- CoverageConfig struct + container-default fns + Config.coverage field + CONFIG_SCHEMA_VERSION const
crates/application/src/config_parse.rs -- parse_config(); fails closed on schema > current
crates/app/tests/config_drift_gate.rs -- all five acceptance-criteria tests for CXA-F021
crates/app/src/lib.rs -- analyzer engine built from full Config literal incl. coverage/schema_version (line ~629)

## Related

docs/CXA-B004-config-initializer.md (step 2 referenced by test comments) - how sections get #[serde(default)] defaults
COX-B043 - governance policy must survive partial documents; drive toward explicit container defaults
CXA-F023 burndown/gate tests that run cargo test -p coxagent-app during boot checks (this red gate once wedged self-heal)
