FOLDER: Configuration
# Coverage field and Config-drift gate (CXA-F021)

**Keywords:** CoverageConfig, coverage, config drift, schema_version, CONFIG_SCHEMA_VERSION, parse_config, coxagent.json, serde(default), fail-closed load, missing field

## Overview

CXA-F021 closed two gaps in per-project configuration. First it fixed a missing `coverage` section on the top-level `Config`: declaring `CoverageConfig` on the struct and adding it to every hand-written struct literal so the workspace compiles (the exact failure CXA-B004 predicted). Second it added a **config-drift gate**: when a persisted `coxagent.json` carries a `schema_version` newer than this build understands, load refuses it instead of silently defaulting it away. For anyone changing which sections or fields `coxagent.json` carries — or bumping its schema version — this page explains where those knobs live and how load keeps them fail-closed.

## How it works

The single source of truth for a project's config is `coxagent_application::config::Config`, defined at `crates/application/src/config.rs:678`. It is pure data serialized as `coxagent.json`. It declares nine sections, all under `#[serde(default)]`, including:

```rust
pub struct Config {
    pub schema_version: u32,
    pub engine: EngineMapping,
    // ... git, workflow, architecture ...
    pub policy: PolicyConfig,
    pub deploy: DeployConfig,
    pub releases: ReleasesConfig,
    pub coverage: CoverageConfig,
}
```

Two mechanisms make up CXA-F021.

1. **Missing-field fix (compile side).** Rust requires every non-spreaded struct literal to list every field exactly once. When `coverage` was added to the struct body without updating each literal initializer at once, compilation failed with E0063 "missing field 'coverage'". The fix adds both new fields to every full literal:
   - The hub builds one full literal inside its call to `build_engine(...)` in `run_hub`, at about line 629 of `crates/app/src/lib.rs`, adding:
     ```rust
     coverage: coxagent_application::config::CoverageConfig::default(),
     schema_version: coxagent_application::config::CONFIG_SCHEMA_VERSION,
     ```
   - Two release-pipeline literals use functional-update syntax (`..Config::default()`), so they survive future field additions without edits.
   - Everything else reaches config through the loading adapter below.

2. **Config-drift gate (load side).** Loading flows through three layers that together refuse an incompatible document rather than guessing:

   ```
   crates/app/src/config_load.rs :: load_config_with_probe(state_dir)
     -> reads <workspace-root>/coxagent.json
     -> coxagent_application::config_parse :: parse_config(&text)
          deserialize into Config (serde_path_to_error names the broken field),
          then check cfg.schema_version > CONFIG_SCHEMA_VERSION
              -> Err(ConfigParseError { field: "schema_version", detail: "persisted schema N is newer than supported M; upgrade coxagent" })
   ```

   The drift check lives in [`parse_config`](crates/application/src/config_parse.rs) at lines 65-73 of that file. A document written by a NEWER build (`schema_version > CONFIG_SCHEMA_VERSION`) is refused — never accepted and never re-defaulted into this build's view of defaults.

An omitted coverage section applies documented defaults (`enabled = true`, `threshold = 3`) produced by an explicit container default rather than Rust's derived zero-value (`{false, 0}`), which would silently misrepresent an unset knob (COX-B043).

## Usage

Verify the workspace still compiles after any config-surface change:

```
cargo check --workspace        # must pass cleanly
```

Run this ticket's TDD contract:

```
cargo test -p coxagent-app --test config_drift_gate
```

Locate every place that constructs a full (non-spreaded) `Config { ... }` so you update them together when adding or removing a section:

```
grep -rn ': Config {' crates/
```

Add a new setting correctly:

1. Declare its type and default in `crates/application/src/config.rs`.
2. Give every scalar its own standalone default via a named function used by serde — e.g. `default_coverage_enabled()` / `default_coverage_threshold()`.
3. If used by the hub analyzer path, add it to both sides of the literal at about line 629 of `crates/app/src/lib.rs`.
4. Re-run both checks above before pushing.

Bump the persisted schema correctly:

1. Increment [`CONFIG_SCHEMA_VERSION`](crates/application/src/config.rs) from 1.
2. Ensure older persisted documents still load through today's parser so user-set values survive migration (AC5): absent sections become defaults; present user-set values like an explicit threshold are preserved verbatim.
3. Keep refusing any NEWER version — nothing above your constant may ever be read blindly.

## Interface

Public surface added / changed by this ticket:

- `CoverageConfig` — nested section struct in `crates/application/src/config.rs`:
  | Field | Type | Default |
  |-------|------|---------|
  | `.enabled` | bool | true, via `default_coverage_enabled()` |
  | `.threshold` | u32 | 3, via `default_coverage_threshold()` |

- `CONFIG_SCHEMA_VERSION : u32 = 1` (`crates/application/src/config.rs`) — how many versions this build understands; raised only on incompatible changes.
- Top-level `Config.schema_version : u32`, defaulted via `current_schema_version()`.

Parsing entry point (`crates/application/src/config_parse.rs`):

```rust
pub fn parse_config(text: &str) -> Result<Config, ConfigParseError>;
pub struct ConfigParseError {
    pub field: String /* dotted path of offending value */,
    pub detail: String,
}
pub const WHOLE_DOCUMENT: &str = "<document>"; // when text is not JSON at all
```

Loading adapter (`crates/app/src/config_load.rs`, reads `<root>/coxagent.json`, heals bad deploy ports):

```rust
pub(crate) fn load_config_with_probe(state_dir: &Path)
    -> Result<LoadedConfig /* .config + .host_port_probe */ , String>;
pub(crate) fn load_config(state_dir: &Path) -> Result<Config, String>;
```

Drift-gate behaviour (`parse_config`, lines 65-73): accepts documents whose embedded version equals or predates this build; rejects anything newer with an error telling the operator to upgrade rather than naming an editable file key.

## Configuration

There is no CLI flag or environment variable for either half of this ticket; all behaviour derives from serde attributes plus constants in code.

| Setting / constant | Where defined | Behaviour |
|---|---|---|
| `.coverage.enabled = true` | named serde default on `CoverageConfig` in `crates/application/src/config.rs` | master switch for coverage-gap detection (no consumer wired yet; see Edge cases) |
| `.coverage.threshold = 3` (u32) | `default_coverage_threshold()` on `CoverageConfig` | coverage percentage below which a gap is filed, once a consumer lands |
| `.schema_version == CONFIG_SCHEMA_VERSION (=1)` | top-level serde attribute via `current_schema_version()`; constant in `config.rs` | fresh docs carry it; older docs omit it and deserialize to current before being checked inside the drift gate — accepting equal-or-prior, rejecting newer outright |

One convention governs every scalar field regardless of which half you touch (parity with COX-B043): **each must carry its own standalone serde default**, so an omitted section stays distinct from corrupt data — absent is not discarded governance policy.

## Edge cases and limits

- A document written by a FUTURE build (`schema_version > CONFIG_SCHEMA_VERSION`) is refused at load outright, naming `field = "schema_version"` and telling the operator to upgrade. It is never partially accepted nor re-defaulted into this build's defaults.
- An OLDER / hand-written document loads fine precisely because every section is serde-defaulted; absence of a section is not corruption (COX-B043).
- No `coxagent.json` on disk at all is not a config error: it loads as full defaults with nothing for any probe (the NotFound branch in `load_config_with_probe`, `crates/app/src/config_load.rs`).
- Only genuinely unrepresentable values fail other parts of parse; a malformed neighbour does not wipe governance policy — the same COX-B043 posture enforced by tests such as `one_malformed_field_never_empties_the_governance_policy`.
- **There is no runtime feature behind `coverage` yet.** As with CXA-B004, the word appears only in unrelated test fixtures inside approval-risk scoring; no gap-detection loop consumes `.coverage.enabled` / `.coverage.threshold` today. This ticket lands configuration plumbing + drift enforcement + round-trip/migration tests; actual gap-filing consumers are anticipated by AC wording but not implemented.
- Hot reload (AC4) is proven by cyclic re-reads of the file at each cycle boundary — there is no daemon pushing changes; persistence plus a fresh read per pass makes a threshold change take effect without a restart.
- Line numbers drift constantly (~1697 lines in `lib.rs` when written); locate symbols (`build_engine`, `parse_config`) rather than line numbers, since E0063 reappears if you add another section to only one side of a literal.
- Migration preserves explicit user-set values verbatim (e.g. an explicit threshold) instead of fabricating back to defaults — verified by AC5's prior-version case.

## Code map

- `crates/app/tests/config_drift_gate.rs` — TDD contract for CXA-F021 (written before implementation): five acceptance-criterion tests for round-trip, omitted-field defaults, newer-schema refusal, hot reload via re-read, and prior-version migration.
- `crates/application/src/config.rs` — authoritative definition of `Config` incl. new `coverage: CoverageConfig`, the nested struct, its named defaults (`default_coverage_enabled()`, `default_coverage_threshold()`), `CONFIG_SCHEMA_VERSION`, and unit tests including round-trip through JSON.
- `crates/application/src/config_parse.rs` — serde-based parse from JSON text into Config that names the broken field via serde_path_to_error; hosts the config-drift check at lines 65-73 refusing any schema newer than supported.
- `crates/app/src/lib.rs` — hub bootstrap; builds one full non-spreaded `&Config { ... }` literal inside its `build_engine(...)` call (about line 629), the site that needs both added fields (`coverage`, `schema_version`) to keep compiling.
- `crates/infrastructure/src/state/json_store.rs` / `sql_store.rs` — analogous fail-closed schema gates for project state, which this config gate mirrors.

## Related

- cxa-b004-missing-coverage-field-in-config.md (Configuration) — documents the same top-level Config type and the "missing coverage field" E0063 build error this ticket resolved by declaring + wiring the field across every hand-written literal site.
- CXA-B004 / COX-B043 — "a malformed field must not wipe the whole config": absent section is not a corrupt document; governs load failure semantics this drift gate extends.
- COX-B042 / COX-B053 — deploy host-port validation enforced by `load_config_with_probe`, which shares this load path.
- cxa-b001-docker-compose-deploy-failure.md (Deployment) — deploy.host_port handling flows through the same loading adapter.
