FOLDER: Configuration
# Config Struct and the missing "coverage" field build error (CXA-B004)

**Keywords:** Config, coxagent.json, struct initializer, missing field, coverage, build failure, config_load, lib.rs, EngineMapping

## Overview

CXA-B004 was filed as a build break: "Build fails: missing coverage field in Config struct initializer at lib.rs line 617." This page documents the project's top-level configuration type named Config (persisted as coxagent.json) and what investigating that ticket found. Anyone landing here from a "missing field in Config initializer" error - or looking to change which sections coxagent.json carries - will find where the type is defined, every place it is constructed by hand, and why adding or removing a section does or does not fail the build.

## How it works

The single source of truth for a project's runtime configuration is Config,
defined in crates/application/src/config.rs . It is pure data (no IO);
loading from disk is an adapter concern in crates/app/src/config_load.rs .

The type currently declares exactly seven fields (all under serde default,
so an older or hand-written document that omits a section still loads):

```
pub struct Config {
    pub engine: EngineMapping,
    pub git: GitConfig,
    pub workflow: WorkflowConfig,
    pub architecture: Vec<crate::conformance::StackRule>,
    pub policy: PolicyConfig,
    pub deploy: DeployConfig,
    pub releases: ReleasesConfig,
}
```

There is NO coverage field. A search across full git history
(git log --all -S "coverage:" ) returns nothing for this type; one was never
added and later removed. The only other occurrences of "coverage" in the repo
are doc comments about test coverage.

Because Rust requires every struct literal to list all fields unless it ends
with ..Default::default() , a hand-written &Config { ... } that forgets one
fails to compile with exactly the error CXA-B004 names. The flow for anyone
reading this ticket:

1. The full-featured hub constructs one literal at line 629 of
   crates/app/src/lib.rs , inside run_hub within its call to build_engine(...).
   It lists all seven fields and matches the type.
2. Two other literals exist only in release-pipeline tests:
   run_releases_tests.rs and run_releases_tdd_tests.rs under
   crates/application/src/use_cases/. Each uses ..Config::default() , so they
   survive any future field being added.
3. Everything else reaches config through load_config_with_probe(state_dir),
   which serde-deserializes into LoadedConfig holding both config and
   host_port_probe. An absent section becomes its default; only a value that
   cannot be represented fails the load (COX-B043).

At verification time ( cargo check -p coxagent-app ) the workspace compiles
cleanly; no missing-field error reproduces on HEAD.

## Usage

To see whether any real missing-field error exists on your branch:

```
cargo check -p coxagent-app     # compile against your current tree
```

Confirm there is no coverage anywhere in Config history:

```
git log --all -S "coverage:" --oneline -- crates/application/src/config.rs crates/app/src/lib.rs
```

Locate every hand-written initialization of each config section:

```
grep -rn ': Config {' crates/
grep -n 'fn run_hub' crates/app/src/lib.rs     # home of lib.rs's &Config { ... }
```

If you add or remove a section from Config , fix EVERY non-default-spreaded
literal at once ( lib.rs line 629 plus any new ones ); otherwise those sites
fail with precisely this ticket's symptom.

## Interface

- Type struct Config lives at crates/application/src/config.rs with fields:
  engine : EngineMapping ; git : GitConfig ; workflow : WorkflowConfig ;
  architecture : Vec of StackRule ; policy : PolicyConfig ;
  deploy : DeployConfig ; releases : ReleasesConfig .
- Nested sections defined beside it:
  EngineMapping has default of Claude/sonnet with auto-failover on.
  GitConfig / WorkflowConfig / PolicyConfig / DeployConfig / ReleasesConfig ,
  each paired with helper defaults such as default_true() ,
  default_provider() , etc.
- Loading adapter load_config_with_probe(&Path) -> Result<LoadedConfig>
  lives at crates/app/src/config_load.rs ; LoadedConfig holds both config and host_port_probe .
- Entry points that load config per project or hub worker:
  operator_main -> run_loop -> load_config_with_probe ; also build_project .

## Configuration

Members of enum EngineKind include Opencode / Claude / Hermes / Gemini /
Codex / Copilot / Scripted . Section-level defaults keep partial documents loadable:

| Field | Default when omitted |
|---|---|
| Every top-level section | its per-section Default impl |

No setting named or keyed by coverage exists anywhere; there is nothing to configure on that front.

## Edge cases and limits

- This ticket describes a state that does NOT reproduce on HEAD. The two commits tagged CXA-B004 (d26fa13 and c43c8c9) carry unrelated content - an autoheal watchdog script plus config-load and PR-review-gate test changes - so their labels are misleading; neither touched a coverage definition.
- Line numbers drift constantly (the file was roughly 1695 lines at writing). Never treat lib.rs line number as stable truth; always recompile against your own tree.
- Only missing named fields break these literals. Adding fields under serde default changes deserialization but not construction correctness of existing spread-style initializers.
- Legacy or handwritten documents load by design: serde default makes an absent section distinct from corrupt data (see COX-B043).

## Code map

- crates/application/src/config.rs -- owns structs for every configuration knot point including Config itself.
- crates/app/src/config_load.rs -- loads coxagent.json into LoadedConfig (with host-port probe); handles legacy/healed documents.
- crates/app/src/lib.rs -- hosts run_hub with the sole full-featured &Config { ... } literal at line ~629.
- crates/application/src/use_cases/run_releases_tests.rs and run_releases_tdd_tests.rs -- release-pipeline tests building Config via spread default.

## Related

- CXA-B043 / COX-B043 -- "a malformed field must not wipe the whole config": absent section != corrupt document; governs load failure semantics here.
- cxa-b001-docker-compose-deploy-failure.md (Engineering / Deployment) -- deploy.host_port handling flows through load_config_with_probe; same loading adapter, different surface.
- CXA-B042 -- host-port expiry/healing logic in config_load used by run_loop.
