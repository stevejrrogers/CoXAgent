FOLDER: Configuration
# Coverage Policy & Config-Drift Gate

**Keywords:** coverage, CoverageConfig, threshold, gap detection, CXA-F021, schema_version, CONFIG_SCHEMA_VERSION, config drift, coxagent.json, hot reload

## Overview

CXA-F021 fixes a missing piece of the persisted configuration surface and adds a compile-time gate against configuration drift. Before this ticket there was no `coverage` section on the top-level [`Config`], so gap-detection settings could never be round-tripped through disk and an unset knob silently deserialized to Rust's zero value (`{false, 0}`) instead of a documented default. It also introduces a schema anchor ([`CONFIG_SCHEMA_VERSION`]) so a `coxagent.json` written by a future build is refused at load rather than accepted blind or re-defaulted away. This is for operators editing project config and for any future pass that consumes coverage policy.

## How it works

The feature has three moving parts:

1. **The config surface.** [`CoverageConfig`] (`enabled`, `threshold`) is added to the top-level [`Config.coverage`] with `#[serde(default)]`, exactly like every sibling section. Its defaults are produced by an explicit container [`Default for CoverageConfig`] plus per-field serde defaults (`enabled = true`, `threshold = 3`) — never Rust's derived zero-value. This is COX-B043's rule applied to one more section.

2. **The schema anchor.** [`CONFIG_SCHEMA_VERSION = 1`] names what this build understands. A document carrying a higher numeric `schema_version` was written by a newer build; loading it would map fields this build cannot represent onto today's defaults.

3. **The refuse-at-load gate.** [`parse_config()`] first parses the document as raw JSON and checks its top-level numeric `schema_version`. If it exceeds [`CONFIG_SCHEMA_VERSION`], parsing returns a [`ConfigParseError`] naming field `schema_version`, before serde ever maps it onto types. A document with no marker predates/matches this build and loads normally — absence is not corruption (COX-B043).

End-to-end load flow:

```
runner boot / Settings save
        │
        ▼
app/src/lib.rs ── load_config_with_probe(state_dir)      [adapter]
        │              └─ reads <state-dir-parent>/coxagent.json
        ▼                             │ fs::read_to_string → text
app/src/config_load.rs                ▼
   .config                    parse_config(text)           [pure decision]
                                      │  header schema_version check →
                                      │    Err("schema_version") if future build wrote it
                                      ▼          else serde → Config (coverage filled)
                              health port probe + self-heal host_port
```

Hot-reload path (a Settings edit reaching the next cycle without restart): each worker-cycle iteration in the runner loop computes [`config_content_hash()`](lib.rs) from the file; when it differs from the last-seen hash it calls [`load_config_with_probe()`], rebuilds the engine via [`build_engine()`], and applies the new config through [`uc.reload()`]. So a threshold saved from the dashboard is observed by the very next pass.

## Usage

**Enable/pin coverage in your project's root coxagent.json:**

```jsonc
{
  "coverage": {
    "enabled": true,
    "threshold": 3   // see Edge cases: exact unit of `threshold` is not yet pinned
  }
}
```

**Omit it entirely** to get documented defaults — load succeeds with enabled=true, threshold=3:

```jsonc
{ "git": { "repo": "owner/repo" } }   // coverage fills as {true, 3}
```

**Write partial sections safely** — only what you set survives; omitted subfields get their own default:

```jsonc
{ "coverage": { "enabled": false } }   // loads as { false, threshold=3 }
```

**A future schema must be refused**, not loaded blind:

```jsonc
{ "schema_version": 2 }                // refused when CONFIG_SCHEMA_VERSION == 1 → error naming "schema_version"
```

Run the acceptance tests:

```
cargo test -p coxagent-app --test config_drift_gate
```

## Interface

Public API added/changed in this ticket:

| Item | Location | Notes |
|------|----------|-------|
| [`struct CoverageConfig { enabled: bool; threshold: u32 }`](crates/application/src/config.rs) | domain-ish data type | both fields carry their own serde default fn |
| `pub const CONFIG_SCHEMA_VERSION: u32 = 1;` | same file | bump when serialized shape changes incompatibly |
| `pub struct Config { … pub coverage: CoverageConfig … }` | same file | marked `#[serde(default)]`, so an omitted section loads |
| helper fns | same file | default_coverage_enabled()→true, default_coverage_threshold()→3 |
| [impl Default for CoverageConfig] | same file | explicit container default per COX-B043 |

Load / parse API unchanged in signature but behaviour-extended:

- coxagent_application::config_parse::parse_config(&str) -> Result<Config, ConfigParseError> — now refuses future schema.
- crate::ports / app adapter load_config_with_probe(state_dir) -> Result<LoadedConfig,String> (app side).

Payload shape on disk (`coxagent.json`) relevant fields:
```
schema_version   optional integer ≥0   higher than supported ⇒ refuse load.
coverage.enabled bool                  default true.
coverage.threshold uint                default 3.
```

## Configuration

Settings that change behaviour (all under Project > Settings, persisted to the root `coxagent.json`):

| Setting | Default | Effect |
|---------|---------|--------|
| `schema_version` (top-level integer) | absent ⇒ treated as prior/matching build; `> CONFIG_SCHEMA_VERSION` ⇒ load refused, error names `schema_version` | config-drift gate |
| `coverage.enabled` | `true` | whether gap-detection proposes coverage chore tickets at all (CXA-F007 consumer wiring) |
| `coverage.threshold` | `3` | how large a gap must be before a module is flagged; **unit is unpinned** — config.rs's doc comment says "minimum gap-free depth (in cycles)", while the TDD contract (config_drift_gate.rs) says "uncovered functions per module". Resolve when CXA-F007 wires the consumer. |

There are no new env vars or CLI flags introduced by this ticket; host-port healing and the deploy health gate live under the pre-existing `deploy.*` settings.

## Edge cases and limits

- **Refuses only NEWER schemas.** A document whose version matches or predates [`CONFIG_SCHEMA_VERSION`] always loads; migrating older shapes relies on per-field serde defaults filling absent subfields, not an explicit migration table in this ticket.
- **No runtime consumer yet.** CXA-F021 lands only the settings *surface* — types, defaults, and the drift gate. No runtime pass reads `.coverage.enabled/.threshold today outside tests; wiring that consumption belongs to CXA-F007 / follow-up work referenced in docs/CXA-B004-config-initializer.md.
- **Malformed marker.** A top-level object that is not valid JSON fails parse with field `<document>`; a non-numeric / non-object `schema_version` is simply not treated as a future version (only an integer strictly greater than [`CONFIG_SCHEMA_VERSION`] triggers refusal).
- **Threshold unit not pinned.** The on-disk value round-trips and defaults correctly, but its meaning is contested in code: `CoverageConfig`'s doc comment (config.rs) calls it "minimum gap-free depth (in cycles)", while config_drift_gate.rs describes it as "uncovered functions per module". No runtime consumer exists yet, so both readings load fine; CXA-F007 must settle one before wiring.
- **Never rewrites on refusal.** When schema drift or any unrepresentable value breaks load, it aborts naming the field; it never falls back to [`Config::default()`], which would silently empty governance allowlists/budgets (COX-B043).

## Code map

- `crates/application/src/config.rs` — the surface: [`CoverageConfig`], its explicit container `Default` + per-field serde default fns (`default_coverage_enabled`, `default_coverage_threshold`), [`CONFIG_SCHEMA_VERSION = 1`], and the `pub coverage: CoverageConfig` field on top-level [`Config`]. Its unit-test module covers default engine mapping, JSON round-trip, and omitted-section handling.
- `crates/application/src/config_parse.rs` — the drift gate: `parse_config()` parses the raw JSON header first and refuses a `schema_version` strictly greater than supported (inlined, no separate helper fn), returning a [`ConfigParseError`] naming field `schema_version`. Includes malformed-document / bad-field tests.
- `crates/app/src/config_load.rs` — IO adapter: reads `<state-dir-parent>/coxagent.json`, calls `parse_config`, self-heals host_port, derives the deploy health probe (`load_config_with_probe`, `heal_host_port`, `probe_from_raw`) with its own config-load tests.
- `crates/app/src/lib.rs` — composition root + hot reload: constructs the hub-level Config incl. the new coverage field (~line 646); the runner loop computes `config_content_hash(state_dir)` each cycle and on change runs `load_config_with_probe(state_dir)` + rebuild engine via (`build_engine`) then applies through (`uc.reload`) so a Settings edit lands next cycle without a restart.
- crates/app/src/builders.rs — mirrors the same hash+reload detection for engine reuse during builds.

## Related

- docs/CXA-B004-config-initializer.md (engineering/configuration) — COX-B043 rule this section follows; "every section is #[serde(default)]".
- Ticket CXA-F007 — gap-detection consumer that will read `.coverage.enabled/.threshold`.
- crates/app/tests/config_drift_gate.rs — TDD contract mapping AC1–AC5 of CXA-F021 to these five tests.
- wiki Product pages Project Activation, Analytics (Cycle Performance Dashboard), Incidents for sibling feature-area context.
