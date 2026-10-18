FOLDER: Configuration

# Coverage field and Config-drift gate

**Keywords:** CoverageConfig, coverage, config drift, schema_version, CONFIG_SCHEMA_VERSION, parse_config, coxagent.json, serde(default), fail-closed load, missing field

## Overview

CXA-F021 fixed a missing `coverage` section on the top-level project config and added a config-drift compile gate so a persisted `coxagent.json` written by a newer build is refused at load instead of silently defaulted into this build's view of defaults. It is for anyone adding or removing sections from `coxagent.json`, bumping its schema version, or wiring the gap-detection coverage knob to a real pass.

## How it works

The single source of truth for per-project configuration is `coxagent_application::config::Config`, serialized as `coxagent.json` (crates/application/src/config.rs:690). Every section — including the new one — is `#[serde(default)]`, so an older or hand-written document that omits a section still loads with defaults.

Two mechanisms make up CXA-F021:

1. **Missing coverage field (compile side).** Rust requires every non-spreaded struct literal to list every field. Adding `coverage` to the struct body without updating each literal fails with E0063 "missing field". The fix adds it to every full literal:
   - The hub builds one full literal in `run_hub` (`crates/app/src/lib.rs:646`), adding:
     ```rust
     coverage: coxagent_application::config::CoverageConfig::default(),
     ```
   - Other constructors use functional-update syntax (`..Config::default()`), so they survive future field additions unchanged.

2. **Config-drift gate (load side).** Loading flows through two layers that together refuse an incompatible document rather than guessing:

   ```
   crates/app/src/config_load.rs :: load_config_with_probe(state_dir)
     -> reads <workspace-root>/coxagent.json
     -> coxagent_application::config_parse :: parse_config(&text)
          deserialize into Config via serde_path_to_error (names the broken field),
          then check header.schema_version > CONFIG_SCHEMA_VERSION
              -> Err(ConfigParseError { field: "schema_version",
                    detail: "persisted schema N is newer than supported M; upgrade coxagent" })
   ```

   The drift check lives in [`parse_config`](crates/application/src/config_parse.rs:47-74). `schema_version` is **not** a field on `Config` — it is read from the raw JSON header before deserialization and compared against [`CONFIG_SCHEMA_VERSION = 1`](crates/application/src/config.rs:647). A document whose version exceeds that constant is refused; one that omits the key predates the anchor and loads as prior-version state.

An omitted coverage section applies documented defaults (`enabled = true`, `threshold = 3`) produced by an explicit container default rather than Rust's derived zero-value (`{false, 0}`), which would silently misrepresent an unset knob (COX-B043).

## Usage

Run this ticket's TDD contract:

```
cargo test -p coxagent-app --test config_drift_gate
```

Verify no compile regression after any config-surface change:

```
cargo check --workspace        # must pass cleanly
```

Locate every place constructing a full (non-spreaded) `Config { ... }` so you update them together when adding or removing a section:

```
grep -rn ': Config {' crates/
```

Add or change a setting correctly:

1. Declare its type + default in `crates/application/src/config.rs`.
2. Give each scalar its own named serde default function (e.g. `default_coverage_enabled()`, `default_coverage_threshold()`).
3. If it feeds the hub analyzer path, mirror it into the literal at about line 646 of `crates/app/src/lib.rs`.
4. Re-run both checks above before pushing.

Bumping the persisted schema:

1. Raise [`CONFIG_SCHEMA_VERSION`](crates/application/src/config.rs:647).
2. Older documents still load because omitting the key means "prior version".
3. Documents written by builds *newer* than yours are now refused at load — there is no forward-migration path; operators must upgrade.

Example accepted document with coverage set:

```json
{
  "schema_version": 1,
  "coverage": { "enabled": false, "threshold": 7 }
}
```

## Interface

- [`pub struct CoverageConfig { pub enabled: bool; pub threshold: u32 }`](crates/application/src/config.rs) — gap-detection policy; both fields public.
- [`impl Default for CoverageConfig`](crates/application/src/config.rs) → enabled=true, threshold=3.
- [`pub const CONFIG_SCHEMA_VERSION: u32 = 1`](crates/application/src/config.rs) — version of persisted schema this build understands.
- [`pub fn parse_config(text) -> Result<Config, ConfigParseError>`](crates/application/src/config_parse.rs) — returns error naming offending field when JSON invalid/unrepresentable; refuses docs whose header schema_version exceeds CONFIG_SCHEMA_VERSION.
- [`pub struct ConfigParseError { pub field; pub detail }`](crates/application/src/config_parse.rs) — when text isn't JSON at all, field equals `<document>` ([WHOLE_DOCUMENT](crates/application/src/config_parse.rs)).
- Private serde default helpers in `config.rs`: `default_coverage_enabled() -> bool { true }`, `default_coverage_threshold() -> u32 { 3 }`.

## Configuration

The only user-facing knobs introduced here live under each project's top-level `coverage:` section in its serialized `coxagent.json`. There are no environment variables or CLI flags; defaults come from the explicit container-default functions, never Rust's derived zero-value.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `coverage.enabled` | bool | `true` (from `default_coverage_enabled()`) | Whether gap-detection coverage gating runs on/off |
| `coverage.threshold` | u32 | `3` (from `default_coverage_threshold()`) | Minimum gap-free depth (in cycles) before a codebase stops being flagged |

Schema-gate constant:

| Setting | Value |
|---------|-------|
| Supported max persisted schema (`CONFIG_SCHEMA_VERSION`) | hard-coded **1** |

There are no configuration files, env vars, or CLI flags for the drift gate — its threshold is the compile-time constant and cannot be tuned at runtime.

## Edge cases and limits

- **No forward migration.** A doc from a future build (`schema_version > CONFIG_SCHEMA_VERSION`) is refused at load with an error naming the mismatch; operators must upgrade rather than force-load.
- **Omitted key means "prior version".** A document that never wrote the anchor loads fine; there is no separate older-version registry to consult.
- **Omitted coverage field ≠ garbage.** Omitting both values yields documented defaults (`enabled=true`, threshold=3); a *present but unrepresentable* value fails rather than defaulting away (COX-B043 posture).
- **No backward-migration branch yet.** Because prior docs simply omit sections and every field has a serde default, prior-version state migrates by preserving user-set fields verbatim — no dedicated migration code path exists.
- **Consumers not yet wired.** As of this landing only tests read `.coverage.enabled/.threshold`. No production gap-detection pass consumes these knobs yet; wiring them into such a pass is follow-up work once one exists.
- The same load path does not attempt host-port healing on an unparseable document — it surfaces the parse error instead of rewriting what the operator meant.

## Code map

- `` crates/app/tests/config_drift_gate.rs `` — TDD contract for CXA-F021, written RED before implementation. Five tests map to ACs: AC1 round-trip → `coverage_section_round_trips_exactly_on_disk`, AC2 omitted-field defaults → `omitted_coverage_applies_the_documented_defaults`, AC3 drift gate → `newer_persisted_schema_is_refused_at_load_not_defaulted`, AC4 hot reload → `threshold_change_reaches_the_next_pass_without_a_restart`, AC5 bump migration → `prior_version_state_migrates_and_preserves_user_coverage`.
- `` crates/application/src/config.rs `` — declares [`CoverageConfig`](crates/application/src/config.rs:657), its explicit container [`Default`](crates/application/src/config.rs:672), [`CONFIG_SCHEMA_VERSION = 1`](crates/application/src/config.rs:647), and adds [`pub coverage: CoverageConfig`](crates/application/src/config.rs:713) to top-level [`Config`](crates/application/src/config.rs:690).
- `` crates/application/src/config_parse.rs `` — [`parse_config`](crates/application/src/config_parse.rs:47) reads the raw JSON header and refuses a future schema version (lines 57–74); defines [`ConfigParseError`](crates/application/src/config_parse.rs:21) / [WHOLE_DOCUMENT].
- `` crates/app/src/lib.rs `` — hub literal construction in run_hub adds `coverage: CoverageConfig::default()` (~line 646); imports re-exposed via crate root.
- `` crates/app/src/config_load.rs `` — the load-time adapter (`load_config_with_probe`) that reads `<workspace-root>/coxagent.json` and calls `parse_config`; host-port healing twin of this lives here too.
- `` crates/app/src/host_port.rs`` — separate load-time path that also routes through `coxagent_application::parse_config`, so it inherits the drift gate.
- `` crates/infrastructure`` and presentation wiring reference parse via config re-exports; no new infra code was needed for CXA-F021.

## Related

There is an existing engineering-space page covering the same ticket: docs/wiki/engineering/configuration/cxa-f021-coverage-field-and-config-drift-gate.md. Note it may predate some edits here; this Product-space page reflects the currently committed code.

Other connected pages:

- docs/CXA-B004-config-initializer.md — origin of the "missing coverage field" failure mode that CXA-F021 closes (COX-B043 default-representation rule).
- docs/TEST_COVERAGE_GAP_DETECTION.md — documents the TEST role's surface inventory; related to but distinct from F021's CoverageConfig knob, which is not yet wired into any pass.
- COX-B043 regression context referenced throughout config.rs / config_parse.rs comments.

