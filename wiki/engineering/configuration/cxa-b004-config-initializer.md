FOLDER: Configuration
# Config struct & its inline initializer (CXA-B004 / CXA-F021)

**Keywords:** Config, coxagent.json, serde(default), CoverageConfig, Config struct initializer, lib.rs:617, build fails missing field, E0063, CONFIG_SCHEMA_VERSION, parse_config

## Overview

`Config` (`crates/application/src/config.rs`) is the persisted per-project configuration type serialized as `coxagent.json`. Ticket CXA-B004 reported a compile failure — "missing `coverage` field in Config struct initializer at lib.rs:617" — caused by an inline `Config { ... }` literal in the hub bootstrap being out of sync with the struct body. The fix (CXA-F021) added a real `coverage: CoverageConfig` section to `Config`, gave it explicit documented defaults (`enabled = true, threshold = 3`) rather than Rust's derived zero-value (`{false, 0}`, COX-B043), and added a fail-closed schema anchor. It landed in commit `3f8d8ec`. As of this worktree (`feat/CXA-B040`) the workspace compiles green and all five config-drift gate tests pass. This page documents the resolved shape for anyone changing a config section or touching either side of this literal.

## How it works

The authoritative shape of a project's config lives in exactly one place: `coxagent_application::config::Config`. It now holds **eight** sections:

1. `engine: EngineMapping`
2. `git: GitConfig`
3. `workflow: WorkflowConfig`
4. `architecture: Vec<crate::conformance::StackRule>`
5. `policy: PolicyConfig`
6. `deploy: DeployConfig`
7. `releases: ReleasesConfig`
8. `coverage: CoverageConfig`

Every section carries a standalone [`#[serde(default)]`](crates/application/src/config.rs) so an older or hand-written document that omits it still parses with defaults instead of failing (COX-B043). Parsing goes through [`parse_config()`](crates/application/src/config_parse.rs), which additionally reads the optional top-level `schema_version` marker and refuses — fail-closed — any document whose version is newer than [`CONFIG_SCHEMA_VERSION`](crates/application/src/config.rs) (= 1), never silently re-defaulting it into this build's view.

### Where "lib.rs:617" actually is

In today's file (`crates/app/src/lib.rs`) line 617 falls inside closure code; the relevant object literal sits at **lines 629–647**, inside this call to [`build_engine()`](crates/app/src/lib.rs):

```rust
let analyzer = build_engine(
    &Config {
        engine: coxagent_application::config::EngineMapping {
            default: coxagent_application::config::EngineChoice {
                engine: coxagent_application::config::EngineKind::Opencode,
                model: "bizbrain/DeepSeek-V4-Pro".to_owned(),
            },
            per_role: std::collections::HashMap::new(),
            fallbacks: Vec::new(),
            auto_fallback: true,
            escalation: Vec::new(),
        },
        git: GitConfig::default(),
        workflow: WorkflowConfig::default(),
        architecture: Vec::new(),
        deploy: DeployConfig::default(),
        policy: PolicyConfig::default(),
        releases: ReleasesConfig::default(),
        coverage:
            coxagent_application::config::CoverageConfig
                ::default(), // line 646
    },
    logs_dir(&base),
    None,
)
```

This block spells out every section because it reaches *into* nested fields rather than relying on derived defaults alone.

> **What broke on CXA-B004.** Rust requires a struct literal to list every field exactly once (E0063 when one is missing). Editing this block without first declaring/re-ordering a matching public field on the struct body stops compilation loudly — which is preferable to silently deploying a config whose declared section never takes effect.

Only this cross-project analyzer constructs an inline literal; all other paths ([hot-reload around lib.rs line 1218](crates/app/src/lib.rs)) load from disk via [`load_config_with_probe()`](crates/app/src/config_load.rs) + [`build_engine()`], so they cannot drift out of sync with the struct.

## Usage

The fix needs nothing at runtime for most operators; behaviour changes only when you touch a config section on either side of this seam:

1. Add/modify a field on any section struct in [`config.rs`](crates/application/src/config.rs).
2. If you changed top-level sections in *count* or *order*, update both sides consistently.
3. Run:
   ```bash
   cargo check --workspace
   cargo test -p coxagent-app --test config_drift_gate   # all 5 must pass
   ```
4. Confirm parse still names bad fields (an out-of-range port yields error field `deploy.host_port`, not `<document>`).

Loading path for reference:

```rust
// crates/app/src/config_load.rs
let loaded = load_config_with_probe(state_dir)?; // reads <root>/coxagent.json
let engine = build_engine(&loaded.config, logs_dir(state_dir), mcp.as_ref())?;
```

## Interface

Public surface of top-level [`Config`](crates/application/src/config.rs): every field is `pub`, each carrying a standalone [`#[serde(default)]`]:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EngineKind { Opencode, Claude, Hermes, Gemini, Codex, Copilot, Scripted }
```

The relevant types and constants (all `pub`, all in [`config.rs`](crates/application/src/config.rs)):

| Item | Kind | Notes |
|------|------|-------|
| `Config` | `struct` | eight sections listed under How it works |
| `CoverageConfig` | `struct { enabled: bool; threshold: u32 }` | explicit container [`Default`] → `{ true, 3 }`, never derived zero-value |
| `CONFIG_SCHEMA_VERSION: u32` | const | = 1; newest persisted version this build will load |
| `EngineMapping` / `EngineChoice` / `EngineKind` | struct / struct / enum | engine+model routing; variant used by the hub analyzer is `EngineKind::Opencode`, model `"bizbrain/DeepSeek-V4-Pro"` |
| [`parse_config(&str) -> Result<Config, ConfigParseError>`](crates/application/src/config_parse.rs) | fn | serde deserialization with field-path error reporting |

Related error type ([config_parse.rs](crates/application/src/config_parse.rs)):

```rust
pub struct ConfigParseError {
    pub field: String,
    pub detail: String,
}
// sentinel for a non-JSON document:
pub const WHOLE_DOCUMENT: &str = "<document>";
```

## Configuration

Everything is driven by the serde defaults on each section — there are no CLI flags or env vars specific to this ticket. Two conventions every new section must honour:

1. **Standalone default per field** (`#[serde(default)]`) so an older or hand-written document omitting it still parses (COX-B043).
2. **Explicit container default when a zero-value would misrepresent intent** — see the handwritten [`impl Default for CoverageConfig { enabled = true; threshold = 3 }`](crates/application/src/config.rs), chosen over derive's silent `{false, 0}`.

## Edge cases and limits

- **No runtime feature behind "coverage" yet.** The config *field* exists so the base compiles green; nothing in app logic reads it at runtime as of this worktree (grep of application use-cases turns up only unrelated test fixtures). Wiring it into gap-detection is future work implied by AC4 of CXA-F021.
- **Schema guard is one-way.** Only a *newer* persisted schema_version is refused. An omitted marker means "prior-version state" and loads fine.
- **Line drift.** "lib.rs:617" no longer points at the literal; today it is lines 629–647 of that file. Always locate by symbol (`build_engine(&Config { ... })`, line number will keep moving).
- **Failure mode.** Adding a field to only one side (struct vs inline literal) fails loudly with E0063 — preferable to silently deploying a config whose declared section never takes effect.
- A missing-on-disk file is not an error: it loads as defaults and leaves nothing to probe (`load_config_with_probe`, NotFound branch).

## Code map

- crates/app/src/lib.rs — hub bootstrap; builds the inline default-mapping Config at lines 629–647 for its cross-project analyzer; per-project paths load from disk via adapters.
- crates/app/src/config_load.rs — reads coxagent.json from disk with self-heal on bad deploy ports (`load_config_with_probe`, hot-reload loop).
- crates/application/src/config.rs — authoritative definition of Config + all eight section structs/defaults/tests + CONFIG_SCHEMA_VERSION + CoverageConfig::default().
- crates/application/src/config_parse.rs — fail-closed JSON→Config parse naming the offending field (`parse_config`, WHOLE_DOCUMENT).
- crates/app/tests/config_drift_gate.rs — five acceptance tests (round-trip / omitted-field defaults / newer-schema refusal / hot reload reachability / prior-version migration).

## Related

- CXA-B004 — this ticket (reported build failure); resolved in current tree.
- CXA-F021 — landing ticket that added CoverageConfig + schema anchor + gate tests (commit 3f8d8ec).
- COX-B043 — omitted config sections load with documented defaults instead of failing parse; rule every new field must honour.
- COX-B042 / COX-B053 — deploy host-port validation enforced through load_config_with_probe/heal_host_port.
