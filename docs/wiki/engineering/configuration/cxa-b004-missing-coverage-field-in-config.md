FOLDER: Configuration
# Config Struct and the missing "coverage" field build error (CXA-B004)

**Keywords:** Config, coxagent.json, struct initializer, missing field, coverage, build failure, CoverageConfig, config_load, lib.rs, CXA-F021

## Overview

CXA-B004 was filed as a build break: "Build fails: missing coverage field in Config struct initializer at lib.rs line 617." This page explains why that happens and how it was resolved on HEAD. Anyone landing here from a Rust "missing field ... in initializer" error for the top-level config type — or looking to add/remove a section of coxagent.json — will find where the type is defined, every place it is constructed by hand without a default spread, and when adding a section does or does not fail the build.

## How it works

The single source of truth for a project's runtime configuration is `Config`, defined in crates/application/src/config.rs. It is pure data (no IO); loading from disk is an adapter concern in crates/app/src/config_load.rs via serde deserialization into `LoadedConfig` (which also carries host-port probe state).

As of HEAD (fixed by commit 3f8d8ec), the type declares **eight** sections:

```
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

The eighth section — `CoverageConfig` with `.enabled: bool` and `.threshold: u32`, defaults `enabled = true`, `threshold = 3`, produced by an explicit container `Default` — is what CXA-B004 reported as missing. It was added (with tests) under ticket CXA-F021 so the shared base compiles green.

Why this class of error happens at all:

1. Rust requires every non-spreaded struct literal to list **all** fields; any hand-written `&Config { ... }` that omits one fails with exactly this symptom.
2. There are two kinds of construction sites:
   - The full-featured hub literal inside `run_hub(...)` at crates/app/src/lib.rs (~line 646), passed to `build_engine(...)`. It lists every field explicitly and therefore must be updated whenever a section is added; it currently ends with `coverage:`.
   - Release-pipeline test literals under crates/application/src/use_cases/, which use spread-default construction and survive future field additions automatically.
3. Everything else reaches config through load_config_with_probe / parse_config; an omitted section becomes its documented default instead of failing (unless the persisted schema_version exceeds what this build understands — see Edge cases).

At verification time (`cargo check -p coxagent-app`) the workspace compiles cleanly; no missing-field error reproduces on HEAD.

## Usage

Confirm your tree compiles:

```
cargo check -p coxagent-app
```

Check whether any real missing-field error exists before building:

```
grep -rn ': Config {' crates/
grep -n 'fn run_hub' crates/app/src/lib.rs   # home of lib.rs's &Config { ... } literal
```

If you add or remove a section from `Config`, update **every** non-default-spreaded literal at once (the run_hub literal plus any new ones); otherwise those sites fail with precisely this ticket's symptom.

Run the TDD contract that pins this behaviour:

```
cargo test --test config_drift_gate -p coxagent-app
```

## Interface

Type + nested sections live in crates/application/src/config.rs:

| Field | Kind | Default when omitted |
|---|---|---|
| `engine : EngineMapping` | nested | per-section Default impl |
| `git : GitConfig` | nested | per-section Default impl |
| `workflow : WorkflowConfig` | nested | per-section Default impl |
| `architecture : Vec<StackRule>` | collection | empty (= conformance off) |
| `policy : PolicyConfig` | nested | per-section Default impl |
| `deploy : DeployConfig` | nested | per-section Default impl |
| `releases : ReleasesConfig` | nested | per-section Default impl |
| `coverage : CoverageConfig { enabled : bool ; threshold : u32 }` | leaf values + two helpers default_coverage_enabled / default_coverage_threshold |

Every top-level section is annotated #[serde(default)]; each pair has helper defaults such as default_true(), default_provider(), etc., so partial documents stay loadable.

Loading adapters:
- load_config_with_probe(state_dir: &Path) -> Result<LoadedConfig, String> lives at crates/app/src/config_load.rs.
- parse_config(text: &str) -> Result<Config, ConfigParseError> lives at crates/application/src/config_parse.rs; it also enforces CONFIG_SCHEMA_VERSION.

Constants:
- CONFIG_SCHEMA_VERSION = 1u32 in crates/application/src/config.rs.

Entry points that load config include operator_main -> run_loop -> load_config_with_probe , and build_project .

## Configuration

Members of enum EngineKind include Opencode / Claude / Hermes / Gemini / Codex / Copilot / Scripted . Section-level defaults keep partial documents loadable.

Coverage-specific knobs (added by CXA-F021):

```json
// example minimal coverage section inside coxagent.json
{
  "coverage": { "enabled": true, "threshold": 3 }
}
```

Both keys are optional (#[serde(default)]); omitting them applies enabled=true , threshold=3 . These defaults come from an explicit container [`impl Default for CoverageConfig`](crates/application/src/config.rs) rather than Rust's derived zero-value ({false ,0} ) so an unset knob reads as documented-and-true instead of silently off-with-zero-threshold (COX-B043).

There is no runtime consumer wired yet beyond serialization/tests; gap-detection reads these values only after its pass lands.

## Edge cases and limits

- This ticket described a state that does NOT reproduce on HEAD because fix commit 3f8d8ec landed under label CXA-F021 and added exactly what B004 reported missing.
- Only missing named fields break these literals; adding a field changes deserialization but not correctness of existing spread-style initializers.
- Line numbers drift constantly across releases — never treat the ~lib.rs line number as stable truth; always recompile against your own tree (`cargo check -p coxagent-app`).
- Legacy or handwritten documents load by design: serde default makes an absent section distinct from corrupt data (COX-B043).
- Fail closed for future builds: parse_config rejects a persisted schema_version newer than supported rather than re-defaulting it away (config_drift_gate AC3).

## Code map

- crates/application/src/config.rs -- owns every configuration knot point including Config itself and CoverageConfig (+ CONFIG_SCHEMA_VERSION).
- crates/app/tests/config_drift_gate.rs -- TDD contract pinning coverage round-trip/default/migration/schema-refusal semantics.
- crates/app/src/config_load.rs -- loads coxagent.json into LoadedConfig (with host-port probe); handles legacy/healed documents.
- crates/app/src/lib.rs -- hosts run_hub with the sole full-featured &Config { ... } literal (~line 646).
- crates/application/src/config_parse.rs -- parse_config entry plus schema-version enforcement.
- crates/application/src/use_cases/run_releases_tests.rs and run_releases_tdd_tests.rs -- release-pipeline tests building Config via spread default.

## Related

- CXA-F021 -- "Fix missing coverage field and add a Config-drift compile gate"; landed commit 3f8d8ec which resolved CXA-B004 on HEAD.
- COX-B043 -- "a malformed field must not wipe the whole config": absent section != corrupt document; governs load-failure semantics here.
- cxa-b001-docker-compose-deploy-failure.md (Engineering / Deployment) -- deploy.host_port handling flows through load_config_with_probe; same loading adapter, different surface.
