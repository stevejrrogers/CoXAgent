FOLDER: Configuration
# Coverage field in the Config initializer (CXA-B004)

**Keywords:** Config, coverage, CoverageConfig, coxagent.json, missing field, E0063, struct initializer, lib.rs:617, build fails, serde(default), config drift

## Overview

Ticket CXA-B004 reported a compile failure — "missing `coverage` field in Config struct initializer at lib.rs:617". A new config section (`coverage`) was referenced by an inline `Config { ... }` literal before it was declared on the top-level struct. The fix landed as CXA-F021; today both sides exist and `cargo check --workspace` passes. This page documents that failure mode so any future section addition does not regress.

## How it works

The authoritative shape of a project's config lives in exactly one place: `coxagent_application::config::Config` (`crates/application/src/config.rs`), serialized as per-project `coxagent.json`. Its sections are:

1. `engine: EngineMapping`
2. `git: GitConfig`
3. `workflow: WorkflowConfig`
4. `architecture: Vec<crate::conformance::StackRule>`
5. `policy: PolicyConfig`
6. `deploy: DeployConfig`
7. `releases: ReleasesConfig`
8. **`coverage: CoverageConfig`** (added by CXA-F021)

Rust requires a struct literal to list every field exactly once; this particular literal cannot use functional-update syntax (`..Default::default()`) because it reaches into nested fields (the default engine mapping). So each new section must be declared on the struct AND added to every full struct literal — otherwise rustc stops with:

```
error[E0063]: missing field 'coverage' in initializer of '...'
  --> crates/app/src/lib.rs:<line>
```

That was exactly CXA-B004's failure mode.

### Where "lib.rs:617" actually is

Line numbers drift; locate by symbol, not number. The offending literal sits inside the call to `build_engine(...)` (`crates/app/src/lib.rs`) for the hub-level cross-project analyzer — today lines 629–647:

```rust
let analyzer = build_engine(
    &Config {
        engine: coxagent_application::config::EngineMapping { /* ... */ },
        git: GitConfig::default(),
        workflow: WorkflowConfig::default(),
        architecture: Vec::new(),
        deploy: DeployConfig::default(),
        policy: PolicyConfig::default(),
        releases: ReleasesConfig::default(),
        coverage: coxagent_application::config::CoverageConfig::default(), // line 646
    },
    logs_dir(&base),
    None,
)
```

### The coverage section itself

`CoverageConfig { enabled: bool, threshold: u32 }` (`crates/application/src/config.rs`) models gap-detection coverage policy for scheduled passes — a config surface only; no production pass reads it yet (see Edge cases). Its defaults come from an explicit container-level Default impl, never Rust's derived zero-value:

```rust
impl Default for CoverageConfig {
    fn default() -> Self {
        CoverageConfig {
            enabled: default_coverage_enabled(),     // true
            threshold: default_coverage_threshold(), // 3
        }
    }
}
```

This is deliberate per COX-B043 — an unset knob reads as documented-and-true (`enabled = true`) rather than silently misrepresenting an unset state as off (`{false, 0}`).

Every top-level section also carries its own standalone `#[serde(default)]`, so a document written by an older build or by hand — mentioning only some sections — still loads with defaults for what it omits instead of being rejected as corrupt.

### Schema anchoring (fail-closed load)

To stop a persisted shape newer than this build understands from loading blind or being defaulted away:

- `CONFIG_SCHEMA_VERSION: u32 = 1` (`crates/application/src/config.rs`) anchors what this build knows.
- On load (`parse_config(text)` in `crates/application/src/config_parse.rs`), any document carrying a higher number is refused before deserialization:
  ```rust
  if let Some(schema_version) = header.get("schema_version").and_then(|v| v.as_u64()) {
      if schema_version > u64::from(CONFIG_SCHEMA_VERSION) {
          return Err(/* "...upgrade coxagent" */);
      }
  }
  ```
- A document that omits `schema_version` predates the anchor and loads fine as prior-version state.
- Any value present but unrepresentable names its offending dotted field via serde_path_to_error instead of falling back to defaults.

## Usage

Confirm current state compiles green and run the gate that pins this area:

```bash
cargo check --workspace                          # passes today

# TDD contract for CXA-F021's acceptance criteria:
cargo test -p coxagent-app --test config_drift_gate
```

The pattern for adding any future setting without re-triggering CXA-B004:

1. Add your feature/setting.
2. Declare its section on `Config` (`crates/application/src/config.rs`) with a standalone `#[serde(default)]` so an older/hand-written document omitting it still parses. If Rust's derived zero-value would misrepresent an unset knob (a bool meaning "on"), write an explicit container-level Default like `CoverageConfig`.
3. If the hub analyzer needs it too, add it to the inline literal at `crates/app/src/lib.rs:629-647`.
4. Add acceptance tests beside `config_drift_gate.rs` covering round-trip-on-disk / omitted-field-defaults / newer-schema-refused / next-pass-hot-reload / migration-preserves-user-values.
5. Run both commands above before pushing.

Loading flow reference:

```rust
// crates/app -> load_config_with_probe(state_dir)? reads <root>/coxagent.json,
// then parses via:
let parsed = coxagent_application::config_parse::parse_config(text)?;
```

## Interface

The section added by this ticket (`CoverageConfig` in `crates/application/src/config.rs`):

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | `true` (via `default_coverage_enabled()`) |
| `threshold` | u32 | `3` (via `default_coverage_threshold()`) |

Public field on the top-level struct:

```rust
#[serde(default)]
pub coverage: CoverageConfig,
```

Related public API in this area:

- `CONFIG_SCHEMA_VERSION: u32 = 1` — persisted-schema anchor this build understands.
- `parse_config(&str) -> Result<Config, ConfigParseError>` — JSON text into a `Config`, fail-closed; the entry point used by both app and tests.
- `ConfigParseError { field, detail }` with sentinel `<document>` when the text is not JSON at all.
- Defaults helpers: `default_coverage_enabled()` and `default_coverage_threshold()`.

## Configuration

There is no separate knob for this ticket beyond what Interface lists. The governing convention when adding any section: every field must carry its own standalone default (`#[serde(default)]`) so an older or hand-written document omitting it still parses instead of being rejected as corrupt; and anything whose zero-value would lie about "off vs unset" needs an explicit container-level Default. This is pinned by tests in `config_drift_gate.rs`.

## Edge cases and limits

- **No runtime feature behind `coverage` yet.** The `CoverageConfig` surface is landed and tested, but no production pass reads `cfg.coverage.*` — it is config-only. Do not hunt for a gap-detection subsystem.
- **Line drift:** "lib.rs:617" no longer points at the literal; today it is lines 629–647 of the same file. Always locate by symbol (`build_engine(... & Config { ... })`), not line number.
- **One-sided edits fail loudly.** Adding a field only to the struct or only to a literal fails compilation with E0063 — preferred over silently shipping a config section that never takes effect. Both sides must change together.
- **A missing-on-disk file is not an error**: it loads as defaults and leaves nothing to probe (`load_config_with_probe`, NotFound branch).
- **Newer schema refuses to load**: a document whose `schema_version` exceeds this build's is rejected with "...upgrade coxagent", never accepted or defaulted away (fail-closed).

## Code map

- crates/app/src/lib.rs — hub bootstrap; builds an inline full-struct-literal at lines 629–647 for its cross-project analyzer (this is where CXA-B004 broke).
- crates/application/src/config.rs — authoritative definition of `Config`, all eight section structs (`CoverageConfig` included) and their defaults/tests; defines default helpers and CONFIG_SCHEMA_VERSION.
- crates/application/src/config_parse.rs — serde-based parse from JSON text into Config, fail-closed; refuses future schema versions via CONFIG_SCHEMA_VERSION.
- crates/app/src/config_load.rs — loads coxagent.json from disk with self-heal on bad deploy ports.
- crates/app/tests/config_drift_gate.rs — TDD contract for CXA-F021: round-trip-on-disk, omitted-field-defaults, newer-schema-refused-at-load, next-pass hot reload without restart, migration preserves user coverage.

## Related

- CXA-B004 — this ticket (reported build failure; resolved via CXA-F021 in current tree).
- CXA-F021 — the fix that landed the config surface so the base compiles green.
- COX-B043 — omitted config sections load with defaults instead of failing the parse; the rule every new field must honour.
