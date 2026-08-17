FOLDER: Configuration
# Config struct & its inline initializers (CXA-B004)

**Keywords:** Config, coxagent.json, serde(default), Config struct initializer, lib.rs, build fails, missing field, EngineMapping, load_config_with_probe, build_engine

## Overview

`Config` (`crates/application/src/config.rs`) is the persisted per-project configuration type serialized as `coxagent.json`. Ticket CXA-B004 reported a compile failure — "missing `coverage` field in Config struct initializer at lib.rs:617" — caused by adding a field to an inline `Config { ... }` literal without first declaring it on the struct. As of this writing (worktree on branch `feat/CXA-B026`, tip `f5fbb09`) the workspace compiles cleanly and **no `coverage` field exists anywhere in the config surface**. This page documents the real shape of the type and the exact spot where that error would occur again.

## How it works

The authoritative shape of a project's config lives in exactly one place: `coxagent_application::config::Config` (`crates/application/src/config.rs:639`). It holds seven sections:

1. `engine: EngineMapping`
2. `git: GitConfig`
3. `workflow: WorkflowConfig`
4. `architecture: Vec<crate::conformance::StackRule>`
5. `policy: PolicyConfig`
6. `deploy: DeployConfig`
7. `releases: ReleasesConfig`

Every section carries its own container-level default — either Rust's derived default or an explicit implementation such as turning mutable behaviour off by default (`WorkflowConfig`, `DeployConfig`, `PolicyConfig`, `GitConfig` each have an `impl Default`; `ReleasesConfig.enabled` defaults to false via derive). Each field also carries a standalone attribute on top of these so an old or hand-written document loads with defaults for what it omits rather than failing the parse (COX-B043).
### Where "lib.rs:617" actually is

In today's file (`crates/app/src/lib.rs`) line 617 falls inside closure code just above; the relevant object literal sits at **lines 629–646**, inside this call to `build_engine`:

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
    },
    logs_dir(&base),
    None,
)
```

This block spells out every section because it reaches *into* nested fields (the default engine mapping) rather than relying on derived defaults alone.

> **What broke on CXA-B004.** Rust requires a struct literal to list every field exactly once. If someone edits this block to add e.g.
>
> ```rust
> coverage,
> ```
>
> without first declaring a matching public field on the struct body, rustc stops with something like:
>
> ```
> error[E0063]: missing field 'coverage' in initializer of '...'
>   --> crates/app/src/lib.rs:<line>
> ```
>
> The whole crate fails to build even though runtime behaviour is otherwise unchanged. Both sides must change together.
## Usage

Confirm current state builds (no coverage error present):

```bash
cargo check --workspace    # passes today
```

The steps that would reproduce CXA-B004, and how to do it correctly:

1. Add a feature that needs a new setting (e.g. a per-project code-coverage threshold).
2. Declare the field on `Config` in `crates/application/src/config.rs`, with its own default so omitted documents still load:
   ```rust
   #[serde(default)]
   pub coverage: CoverageConfig,
   ```
3. If it is used by the hub's inline analyzer, also add it to the struct literal at `crates/app/src/lib.rs:629-646`:
   ```rust
   coverage: CoverageConfig::default(),
   ```
4. Run `cargo check --workspace` and the config round-trip tests (below) before pushing.

Loading/usage flow for reference:

```rust
// crates/app/src/config_load.rs
let loaded = load_config_with_probe(state_dir)?;  // reads <root>/coxagent.json
let config = loaded.config;
let engine = build_engine(&config, logs_dir(state_dir), mcp.as_ref())?;
```

## Interface

Public surface of `Config` (`crates/application/src/config.rs:639`), all `pub`, all serde-defaulted:

| Field | Type | Default |
|-------|------|---------|
| `engine` | `EngineMapping` | derived default |
| `git` | `GitConfig` | derived default |
| `workflow` | `WorkflowConfig` | explicit impl (mutable flags off) |
| `architecture` | `Vec<crate::conformance::StackRule>` | empty vec (off) |
| `policy` | `PolicyConfig` | explicit impl |
| `deploy` | `DeployConfig` | explicit impl |
| `releases` | `ReleasesConfig` | derived default |

Supporting types live in the same file: nested sections above plus their per-section fields (`DeployConfig.host_port`, etc.). Parsing entry point: `coxagent_application::config_parse::parse_config(&str) -> Result<Config, ConfigParseError>` (`config_parse.rs:47`) validates that "a value the schema cannot represent fails the load".

## Configuration

All behaviour is driven by settings already listed under Interface — there is no separate knob for this ticket beyond those serde defaults. The one convention to respect when adding any section: **every field must carry a standalone default** (`#[serde(default)]`) so an older or hand-written document omitting it still parses rather than being rejected as corrupt. This is enforced in spirit by tests named around COX-B043 below.

## Edge cases and limits

- **What this page deliberately does NOT cover:** there is no runtime feature behind "coverage". The word appears only in unrelated test fixtures inside approval risk scoring; do not hunt for a coverage subsystem.
- **Line drift:** "lib.rs:617" no longer points at the literal; today it is lines 629–646 of the same file. Always locate by symbol (`build_engine(... & Config { ... })`, not line number).
- **Failure mode:** if you add a field only to one side (literal or struct), compilation fails loudly with E0063 — which is preferable to silently deploying a config whose declared section never takes effect.
- A missing-on-disk file is *not* an error: it loads as defaults and leaves nothing to probe (`load_config_with_probe`, NotFound branch).

## Code map

- crates/app/src/lib.rs — hub bootstrap; builds config inline at lines 629–646 for its cross-project analyzer and callsites (per-project bootstrap) consume via adapters below.
- crates/application/src/config.rs — authoritative definition of Config + all seven section structs and their defaults/tests.
- crates/application/src/config_parse.rs — serde-based parse from JSON text into Config.
- crates/app/src/config_load.rs — loads coxagent.json from disk with self-heal on bad deploy ports.

## Related

- CXA-B004 — this ticket (reported build failure; resolved in current tree).
- COX-B043 — omitted config sections load with defaults instead of failing the parse; the rule every new field must honour.
- COX-B042 / COX-B053 — deploy host-port validation that `load_config_with_probe` and `heal_host_port` enforce.
- Tickets CXA-B017, CXA-B029, CXA-B001, CXA-B015 — deploy/compose credential and port work sharing `Config.deploy`.
