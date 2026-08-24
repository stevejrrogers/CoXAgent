FOLDER: Configuration
# Coverage Config Section and Schema Drift Gate (CXA-F021)

**Keywords:** CoverageConfig, coverage field, schema_version, CONFIG_SCHEMA_VERSION, config drift, compile gate, coxagent.json, parse_config, serde default

## Overview

CXA-F021 lands two things on top-level project configuration (`coxagent.json`): a first-class test-coverage section (`coverage`) that was previously missing from the config surface and broke compilation of any full struct literal that listed every field; and a Config-drift gate that refuses to load a persisted document whose schema version is newer than this build understands. The intent is that coverage-gap detection gets real knobs that round-trip and hot-reload instead of being fabricated from Rust's zero-value when a document omits them — and that an incompatible config written by a future build fails closed at load time rather than being silently re-defaulted away.

## How it works

The single source of truth is `Config` in `crates/application/src/config.rs`, persisted as `coxagent.json`. Every top-level section carries `#[serde(default)]`, so an older or hand-written document that omits one still loads with defaults; only a value the schema cannot represent fails load (COX-B043).

**The coverage section.** A new top-level field on `Config`:

```rust
pub struct Config {
    // ...
    #[serde(default)]
    pub coverage: CoverageConfig,
}
```

`CoverageConfig` carries two knobs whose documented defaults are **enabled = true**, **threshold = 3**. Those defaults come from explicit per-field serde functions (`default_coverage_enabled()`, `default_coverage_threshold()`) plus an explicit container `impl Default for CoverageConfig` — NOT Rust's derived zero-value (`{false, 0}`), because that would silently misrepresent an unset knob as "gap detection off" (COX-B043). A partial section preserves what it declares and fills only what it omits from those named defaults.

Every non-spreaded struct literal must list every field; adding `coverage` therefore required updating the sole full-featured literal inside [`run_hub(...)`](crates/app/src/lib.rs) at ~line 646 (`coverage: ...CoverageConfig::default()`). Spread-style literals elsewhere survive future fields untouched.

**The drift gate.** Loading flows through [`config_parse::parse_config(text)`](crates/application/src/config_parse.rs:47). It maps JSON onto typed fields via serde_path_to_error so any broken value names its dotted path; then it enforces AC3:

```rust
if cfg.schema_version > CONFIG_SCHEMA_VERSION {
    return Err(ConfigParseError { field: "schema_version".to_owned(), detail: ... });
}
```

On-disk versioning works through three pieces:

- The build-time constant each running binary knows: `pub const CONFIG_SCHEMA_VERSION: u32 = 1`.
- A per-document marker on `Config`: `#[serde(default = "current_schema_version")] pub schema_version: u32`.
- Because absent markers become current_schema_version() (= CONFIG_SCHEMA_VERSION), only a document that explicitly writes a HIGHER number trips the check — absence of the marker predates/matches this build and loads normally.

**Wiring status.** The five acceptance-criterion tests in crates/app/tests/config_drift_gate.rs prove the coverage section round-trips exactly on disk (AC1), omitted-field defaults apply (AC2), future-schema documents are refused at load (AC3), threshold edits reach a fresh read of the same file with no restart between write and read (AC4), and prior-version state migrates while preserving user-set values (AC5). As of this writing nothing yet READS `.coverage.enabled/.threshold` to drive actual gap-chore filing — treat these knobs as config-surface groundwork until CXA-F007 wiring lands (see Related / Edge cases).

## Usage

The behavior needs no manual steps; it activates whenever you persist or load a config. To exercise each acceptance criterion from source:

```bash
# Run every drift / coverage gate test
cargo test -p coxagent-app --test config_drift_gate

# Compile-check that every Config literal lists 'coverage' (the missing-field build break)
cargo check -p coxagent-app
```

A minimal hand-written config that omits coverage entirely loads with gap detection ON:

```json
{"engine":{"default":{"engine":"claude","model":"sonnet"}}}
```
parses with coverage.enabled=true and coverage.threshold=3.

Partial sections fill only what's absent:

```json
{ "coverage": { "enabled": false } }
```
parses with enabled=false preserved and threshold defaulting to 3.

Refusing a future document — load returns an error naming field schema_version instead of defaulting away:

```json
{ "schema_version": 2 }
```
→ Err("schema_version: persisted schema 2 is newer than supported 1; upgrade coxagent")

In practice this surfaces through [`config_load::load_config_with_probe(state_dir)`](crates/app/src/config_load.rs) which prefixes any parse failure with `<path>/coxagent.json:` so operators see exactly which file refused to load. When such a refusal happens inside [`run_hub(...)`](crates/app/src/lib.rs) it stops project startup rather than falling back to empty governance policy.

## Interface

Types in crates/application/src/config.rs:

- **struct `Config`** — top-level document type; gained `pub coverage` field (`#[serde(default)]`) plus `pub schema_version: u32`.
- **struct `CoverageConfig`** — `.enabled: bool` (serde default `default_coverage_enabled`) and `.threshold: u32` (serde default `default_coverage_threshold`).
- **impl Default for CoverageConfig** — returns { enabled = true, threshold = 3 } via the named fns.
- **fn `default_coverage_enabled() -> bool`** — returns true.
- **fn `default_coverage_threshold() -> u32`** — returns 3.
- **fn `current_schema_version() -> u32`** — returns CONFIG_SCHEMA_VERSION.
- **const `CONFIG_SCHEMA_VERSION: u32 = 1`** — build-time anchor for the drift gate.

Parser surface in crates/application/src/config_parse.rs:

- **fn `parse_config(&str) -> Result<Config, ConfigParseError>`** — deserializes via serde_path_to_error so any broken value names its dotted path; then enforces schema_version <= CONFIG_SCHEMA_VERSION and refuses anything newer.
- **struct `ConfigParseError { field: String, detail: String }`** — thiserror-derived display is "{field}: {detail}"; field is WHOLE_DOCUMENT ("<document>") when the text is not valid JSON; drift refusal fills field "schema_version".
- **const `WHOLE_DOCUMENT: &str`** — sentinel "<document>" used when no single JSON field can be blamed (the text is not JSON at all).

Adapter surface in crates/app/src/config_load.rs and lib.rs:

- **[load_config(state_dir)](crates/app/src/config_load.rs)** / **[load_config_with_probe(state_dir)](crates/app/src/config_load.rs)** — read + parse coxagent.json through parse_config; prefix failures with "<path>:"; heal host_port; derive the deploy health probe port from the same single read so they never drift apart.
- **[run_hub(...)](crates/app/src/lib.rs)** ~line 630–648 builds the sole full-featured &Config literal (adds both new fields coverage + schema_version); its inner loop hot-reloads config+engine at a cycle boundary when the file changes (~line 1217), so a threshold edit applies without restart.

## Configuration

There are no CLI flags or environment variables for CXA-F021 — everything lives on disk in `coxagent.json`, loaded through [`config_load::load_config_with_probe`](crates/app/src/config_load.rs):

| Setting (JSON key) | Type | Default when omitted | Effect |
|---|---|---|---|
| coverage.enabled | bool | true (`default_coverage_enabled()`) | gap detection ON unless a document opts out explicitly |
| coverage.threshold | u32 | 3 (`default_coverage_threshold()`) | knob below which a coverage gap would be filed (not yet consumed by gate wiring) |
| schema_version | u32 | = CONFIG_SCHEMA_VERSION via current_schema_version(); const = 1 | persisted marker; any explicit value above CONFIG_SCHEMA_VERSION fails load |

Every top-level section of `Config` is under `#[serde(default)]`, so an omitted section loads with that section's default rather than failing parse. Only a value the schema cannot represent fails load (COX-B043).

## Edge cases and limits

Deliberately NOT done:

- A stored `schema_version` LOWER than this build's constant still loads normally — versioning only refuses documents NEWER than the build knows. There is no downgrade guard.
- An absent `schema_version` marker is treated as predating/matching this build and loads fine; absence of the marker is not corruption (COX-B043).
- A partial `coverage` section preserves what it declares and defaults only the rest from named defaults — it never fabricates from Rust's zero-value.

Limits / how it fails:

- Drift refusal returns a hard load-time error naming field "schema_version" ("...upgrade coxagent"); callers must NOT fall back to Config::default() on failure, or they silently drop governance policy.
- The coverage knobs are currently CONFIG-SURFACE ONLY: nothing yet reads `.coverage.enabled/.threshold` to propose gap chores. Until CXA-F007 wiring lands, changing them has no observable effect on ticket filing.
- The "missing coverage field" compile break reproduces if someone adds another field to the full literal at lib.rs:~646 without updating it. Spread-style literals elsewhere need no edits.
- Line numbers drift constantly; treat lib.rs:~646 and config_parse.rs:47 as approximate anchors to recompile against your own tree.

## Code map

- crates/application/src/config.rs — defines top-level Config plus every nested section struct and its defaults; hosts CoverageConfig (.enabled/.threshold), named fns default_coverage_enabled/default_coverage_threshold, current_schema_version(), impl Default for CoverageConfig, and pub const CONFIG_SCHEMA_VERSION = 1.
- crates/application/src/config_parse.rs — parse_config(&str) -> Result<Config, ConfigParseError>: serde_path_to_error deserialization + AC3 schema-drift check (refuse any persisted schema newer than CONFIG_SCHEMA_VERSION); owns ConfigParseError + WHOLE_DOCUMENT const.
- crates/app/src/lib.rs — run_hub(...) builds the sole full-featured &Config literal (~line 630–648), gaining coverage: CoverageConfig::default() and schema_version: CONFIG_SCHEMA_VERSION; hot-reload loop re-reads config at cycle boundaries (~line 1217).
- crates/app/tests/config_drift_gate.rs — TDD acceptance-criterion suite for CXA-F021: round-trip (AC1), omitted-field defaults (AC2), future-schema refusal at load (AC3), hot-reload threshold change observed next pass without restart (AC4), prior-version migration preserving user coverage (AC5).

## Related

- CXA-B004 / cxa-b004-missing-coverage-field-in-config.md (+ docs/wiki/engineering/configuration/CXA-B004-config-initializer.md) — documents the same missing-coverage config-surface work this ticket completes; F021 lands step 2 of that page's config-initializer recipe plus a new schema anchor.
- COX-B043 / CXA-B043 — "a malformed field must not wipe the whole config": absent section != corrupt document governs both load-failure semantics and why defaults use named fns rather than Rust zero-values.


