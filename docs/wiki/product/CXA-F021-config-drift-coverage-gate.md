FOLDER: -
# Coverage config field & Config-drift compile gate (CXA-F021)

**Keywords:** CoverageConfig, coverage, coxagent.json, schema_version, CONFIG_SCHEMA_VERSION, config drift, parse_config, ConfigParseError, serde(default), config migration

## Overview

CXA-F021 added a gap-detection **coverage** policy section to the persisted `coxagent.json` config and gave `coxagent.json` a **schema anchor** that stops a future build's document from being loaded blind by an older binary. It is for operators who tune how aggressively the tool flags codebase coverage gaps (a dashboard knob), and for any agent that persists or reads project config. The acceptance criteria are encoded as five integration tests in `crates/app/tests/config_drift_gate.rs`, all of which pass on this branch.

## How it works

The feature lands entirely on the existing config surface — it adds no new runtime pass; it declares the data and hardens load:

1. [`CoverageConfig`](crates/application/src/config.rs:646) is a two-field struct — `enabled: bool`, `threshold: u32`. Each field has an explicit per-field serde default (`default_coverage_enabled()` / `default_coverage_threshold()`) plus an explicit container [`impl Default`](crates/application/src/config.rs:662). Because defaults come from that container rather than Rust's derived zero-value, an unset knob reads as documented-and-true (`enabled = true, threshold = 3`), never silently as `{false, 0}` (COX-B043).
2. A new top-level field on [`Config`](crates/application/src/config.rs:679): `#[serde(default)] pub coverage: CoverageConfig`. Every section is `#[serde(default)]`, so an old or hand-written document that omits it loads with the documented defaults instead of failing.
3. A schema anchor constant [`CONFIG_SCHEMA_VERSION`](crates/application/src/config.rs:637) (`u32 = 1`). Because `Config` does not deny unknown fields, this version is enforced at parse time rather than as a struct field. [`parse_config()`](crates/application/src/config_parse.rs:47) first parses the raw text to JSON just to read an optional top-level `schema_version`; if it exceeds the constant it returns a [`ConfigParseError`](crates/application/src/config_parse.rs:21) naming field `schema_version`, refusing to load — never accepted and never re-defaulted into this build's view of defaults.

End-to-end flow through real code:

```text
load_config_with_probe(state_dir)        crates/app/src/config_load.rs:57
  └─ parse_config(&text)                 crates/application/src/config_parse.rs:47
       ├─ schema_version > CONFIG_SCHEMA_VERSION ? → Err(ConfigParseError{field:"schema_version"})
       └─ serde_path_to_error::deserialize → Config (incl. coverage)
```

This mirrors how state.json already guards itself via [`parse_checked()`](crates/infrastructure/src/state/json_store.rs:414) and its own [`SCHEMA_VERSION`](crates/application/src/state/mod.rs).

## Usage

Verify the contract locally (all five acceptance tests must pass):

```bash
cd crates/app && cargo test --test config_drift_gate
# 5 passed
```

Hand-writing or editing a project's coxagent.json to tune coverage:

```json
{
  "schema_version": 1,
  "engine": { "default": { "engine": "claude", "model": "sonnet" } },
  "coverage": { "enabled": false, "threshold": 7 }
}
```

Omitting the section entirely still loads fine with defaults (`enabled=true`, `threshold=3`) — same as any other section.

## Interface

Public API re-exported from the crate root (`coxagent_application::lib.rs`):

- Type [`CoverageConfig`](crates/application/src/config.rs:646):
  - `.enabled: bool`
  - `.threshold: u32`
- Constant [`CONFIG_SCHEMA_VERSION`: u32 = 1](crates/application/src/config.rs:637)
- Existing entry point unchanged in shape:
  - [`parse_config(&str) -> Result<Config, ConfigParseError>`](crates/application/src/config_parse.rs:47)
    - New refusal mode when drifting carries `.field == "schema_version"`.
    - Other refusals carry the offending dotted path (e.g. `deploy.host_port`) or `<document>` when text is not JSON.
- Composition-root wiring adds one line at [crates/app/src/lib.rs](crates/app/src/lib.rs):646 inside the inline `Config { ... }` literal passed to engine construction:
  ```rust
  coverage: coxagent_application::config::CoverageConfig::default(),
  ```

No HTTP endpoints or CLI flags are added by CXA-F021.

## Configuration

All settings live under top-level keys inside each project's persisted file:

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `.coverage.enabled` | bool | true | Master switch for gap-detection flagging; when false no codebase is flagged regardless of threshold. |
| `.coverage.threshold` | u32 | 3 | Minimum gap-free depth (in cycles) before a pass stops flagging gaps; set via dashboard write path. |
| top-level `.schema_version` | number (int) | absent unless written | If present and greater than CONFIG_SCHEMA_VERSION (=1), load fails closed with an error instead of loading blind. A document that omits it predates the anchor and loads normally as prior-version state. |

All other sections retain their existing per-section defaults from B004/CXA-B006.

## Edge cases and limits

What CXA-F021 deliberately does NOT do:

- **No consumer yet.** The scope landed only declares and round-trips the data; no runtime gap-detection pass currently reads `.coverage.enabled` / `.coverage.threshold`. AC4 ("hot reload reaches next pass") and AC5 ("bump migration preserves user values") are satisfied today only at parse-round-trip level — changing values persists them but nothing acts on them until such a pass exists.
- **Unknown fields are still tolerated.** `Config` does not deny unknown fields, so unrelated keys in an otherwise-fine document deserialize successfully; the drift guard only refuses a too-new `schema_version`, not unexpected sibling keys.
- **Older documents just get defaults.** A document whose `schema_version` matches (or is absent) loads with whatever per-field serde defaults it omits; there is no explicit migration routine beyond that default-fill. Anything from a future build fails loudly rather than being guessed at.
- When loading fails for drift (`field == "schema_version"`) there is no partial-load fallback nor self-heal rewrite attempt — mirroring COX-B043 fail-closed posture elsewhere.

Failures surface where they occur:

```text
invalid <root>/coxagent.json: schema_version<N> is newer than supported <V>; upgrade coxagent
```

with the config file living beside the state dir (the load path from [`load_config_with_probe`](crates/app/src/config_load.rs:57)).

## Code map

```text
crates/application/src/config.rs            -- CoverageConfig struct, its explicit Default, CONFIG_SCHEMA_VERSION const, and #[serde(default)] coverage field on Config (lines ~630-704)
crates/application/src/config_parse.rs      -- parse_config() + ConfigParseError; the schema_version drift guard lives here (lines 47-91)
crates/application/src/lib.rs               -- re-exports CoverageConfig and CONFIG_SCHEMA_VERSION from the crate root (line ~30)
crates/app/src/lib.rs                       -- composition root; adds coverage: CoverageConfig::default() to the inline Config literal (line 646)
crates/app/src/config_load.rs               -- load_config / load_config_with_probe read coxagent.json and call parse_config; fail-closed on parse errors
crates/app/tests/config_drift_gate.rs       -- the five acceptance tests (AC1 round-trip, AC2 defaults, AC3 drift refusal, AC4 hot reload proxy, AC5 migration proxy)
crates/infrastructure/src/state/json_store.rs -- parse_checked() + SCHEMA_VERSION guard that coxagent.json now mirrors (reference pattern only)
```

## Related

- `docs/wiki/engineering/configuration/cxa-b004-missing-coverage-field-in-config.md` — same config surface; B004 described the missing-field build break and was later completed by F021's landed surface.
- `docs/CXA-B004-config-initializer.md` — spec for how every config section gets its default; F021 follows step 2 for `coverage`.
- CXA-B006 release pipeline / `ReleasesConfig` — sibling section of `Config` sharing the `#[serde(default)]` pattern.
- COX-B043 — policy: a document that fails to deserialize is an error, never `Config::default()`; drives both per-section defaults and fail-closed drift refusal.
