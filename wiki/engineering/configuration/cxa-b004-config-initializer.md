FOLDER: Configuration
# Config struct & its inline initializers (CXA-B004)

**Keywords:** Config, coxagent.json, coverage, CoverageConfig, missing field, E0063, struct initializer, lib.rs:617, serde(default), build fails

## Overview

`Config` (`crates/application/src/config.rs`) is the persisted per-project configuration type serialized as `coxagent.json`. Ticket CXA-B004 reported a compile failure — "missing `coverage` field in Config struct initializer at lib.rs:617" — caused by adding a new section to an inline `Config { ... }` literal used by the hub analyzer before it was declared on the struct. The fix landed as **CXA-F021** (commit `3f8d8ec`, "land config surface so the base compiles green"); today both sides exist and `cargo check --workspace` passes. This page documents how any future section must be added so that error does not recur.

## How it works

The authoritative shape of a project's config lives in exactly one place: `coxagent_application::config::Config` (`crates/application/src/config.rs:679`). It holds eight sections:

1. `engine: EngineMapping`
2. `git: GitConfig`
3. `workflow: WorkflowConfig`
4. `architecture: Vec<crate::conformance::StackRule>`
5. `policy: PolicyConfig`
6. `deploy: DeployConfig`
7. `releases: ReleasesConfig`
8. **`coverage: CoverageConfig`** (added by CXA-F021)

Rust requires a struct literal to list every field exactly once; this particular literal cannot use functional-update syntax (`..Default::default()`) because it reaches *into* nested fields (the default engine mapping). So each new section must be declared on the struct **and** added to every full struct literal — otherwise rustc stops with:

```
error[E0063]: missing field 'coverage' in initializer of '...'
  --> crates/app/src/lib.rs:<line>
```

That was exactly CXA-B004's failure mode.

### Where "lib.rs:617" actually is

The object literal referenced by ticket CXA-B004 sits inside the call to [`build_engine(...)`](crates/app/src/lib.rs) for the hub-level cross-project analyzer at **lines 629–647**. Line numbers drift; locate it by symbol:

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

[`CoverageConfig { enabled: bool, threshold: u32 }`](crates/application/src/config.rs#L646-L652) models gap-detection coverage policy for scheduled passes (a config surface only — no production pass consumes it yet). Its defaults come from an explicit container-level Default impl, never Rust's derived zero-value:

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

This is deliberate per COX-B043 — an unset knob reads as documented-and-true (`enabled=true`) rather than silently misrepresenting an unset state as off (`{false, 0}`).

Every top-level section also carries its own standalone-`#[serde(default)]`, so a document written by an older build or by hand — mentioning only some sections — still loads with defaults for what it omits rather than being rejected as corrupt.

### Schema anchoring (fail-closed load)

To keep a persisted shape newer than this build understands from being loaded blind or defaulted away:

- A module constant anchors what this build knows:
  ```rust
  pub const CONFIG_SCHEMA_VERSION: u32 = 1;   // crates/application/src/config.rs
  ```
- On load ([parse_config](crates/application/src/config_parse.rs#L47-L91)) any document carrying a higher number is refused before deserialization:
  ```rust
  if let Some(schema_version) = header.get("schema_version").and_then(|v| v.as_u64()) {
      if schema_version > u64::from(CONFIG_SCHEMA_VERSION) {
          return Err(/* "...upgrade coxagent" */);
      }
  }
  ```
- A document that omits `schema_version` predates the anchor and loads fine as prior-version state.
- Because deserialization uses default-laden types with fail-closed posture (COX-B043), any value present but unrepresentable names its offending dotted field via serde_path_to_error instead of falling back to defaults.

## Usage

Confirm current state compiles green:

```bash
cargo check --workspace                      # passes today

# TDD gate that pins this area's acceptance criteria:
cargo test -p coxagent-app --test config_drift_gate
```

The pattern for adding any future setting without re-triggering CXA-B004:

1. Add a feature that needs a new setting.
2. Declare its section on [`Config`](crates/application/src/config.rs) with a standalone `#[serde(default)]` so an older/hand-written document omitting it still parses:
   ```rust
   #[serde(default)]
   pub my_section: MySectionCaps,
   ```
   If Rust's derived zero-value would misrepresent an unset knob (a bool meaning "on", e.g.), write an explicit container-level `Default` like `CoverageConfig` has, rather than relying on the derived zero.
3. If the hub analyzer needs it too, add it to the inline literal at [`crates/app/src/lib.rs:629-647`](crates/app/src/lib.rs):
   ```rust
   coverage: coxagent_application::config::CoverageConfig::default(),
   ```
4. Add acceptance tests beside [`config_drift_gate.rs`](crates/app/tests/config_drift_gate.rs) covering round-trip-on-disk / omitted-field-defaults / newer-schema-refused / next-pass-hot-reload / migration-preserves-user-values.
5. Run `cargo check --workspace` and the gate test before pushing.

Loading flow for reference:

```rust
// crates/app/src/config_load.rs -> load_config_with_probe(state_dir)? reads <root>/coxagent.json
let parsed = coxagent_application::config_parse::parse_config(text)?; // entry point, also used by tests
```

## Interface

The section added by this ticket ([`CoverageConfig`](crates/application/src/config.rs#L646-L652)):

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | `true` (via `default_coverage_enabled()`) |
| `threshold` | u32 | `3` (via `default_coverage_threshold()`) |

Public field on the top-level struct (line 703):

```rust
#[serde(default)]
pub coverage: CoverageConfig,
```

Related public API in the same area:

- [`CONFIG_SCHEMA_VERSION: u32`](crates/application/src/config.rs#L637) — persisted-schema anchor this build understands (`1`).
- [`parse_config(&str) -> Result<Config, ConfigParseError>`](crates/application/src/config_parse.rs#L47) — JSON text into a `Config`, fail-closed.
- [`ConfigParseError { field, detail }`](crates/application/src/config_parse.rs#L20) — names the offending dotted field; [`WHOLE_DOCUMENT`](crates/application/src/config_parse.rs#L31) when the text is not JSON at all.
- Defaults helpers: [`default_coverage_enabled()`](crates/application/src/config.rs#L654), [`default_coverage_threshold()`](crates/application/src/config.rs#L658).

## Configuration

There is no separate knob for this ticket beyond what is listed under Interface. The governing convention when adding any section: **every field must carry a standalone default** (`#[serde(default)]`) so an older or hand-written document omitting it still parses instead of being rejected as corrupt; and anything whose zero-value would lie about "off vs unset" needs an explicit container-level Default. This is pinned by tests in [`config_drift_gate.rs`](crates/app/tests/config_drift_gate.rs).

## Edge cases and limits

- **No runtime feature behind `coverage` yet.** The `CoverageConfig` surface is landed and tested, but no production pass reads `cfg.coverage.*` — it is config-only. Do not hunt for a gap-detection subsystem.
- **Line drift:** "lib.rs:617" no longer points at the literal; today it is lines 629–647 of the same file. Always locate by symbol (`build_engine(... & Config { ... })`), not line number.
- **One-sided edits fail loudly.** Adding a field only to the struct or only to a literal fails compilation with E0063 — preferred over silently shipping a config section that never takes effect. Both sides must change together.
- **A missing-on-disk file is not an error**: it loads as defaults and leaves nothing to probe (`load_config_with_probe`, NotFound branch).
- **Newer schema refuses to load**: a document whose `schema_version` exceeds this build's is rejected with `"...upgrade coxagent"`, never accepted or defaulted away (fail-closed, like state.json).

## Code map

- crates/app/src/lib.rs — hub bootstrap; builds `Config` inline at lines 629–647 for its cross-project analyzer (this is where CXA-B004 broke).
- crates/application/src/config.rs — authoritative definition of `Config`, all eight section structs (`CoverageConfig` included) and their defaults/tests; `CONFIG_SCHEMA_VERSION`.
- crates/application/src/config_parse.rs — serde-based parse from JSON text into `Config`, fail-closed; refuses future schema versions.
- crates/app/src/config_load.rs — loads coxagent.json from disk with self-heal on bad deploy ports.
- crates/app/tests/config_drift_gate.rs — TDD contract for CXA-F021: round-trip, omitted-field defaults, newer-schema refusal, hot-reload propagation, migration preservation.

## Related

- CXA-B004 — this ticket (reported build failure; resolved via CXA-F021 in current tree).
- CXA-F021 — the fix that landed the config surface so the base compiles green.
- COX-B043 — omitted config sections load with defaults instead of failing the parse; the rule every new field must honour.
