FOLDER: Deployment
# Reclaimable Compose Project Policy (CXA-F026)

**Keywords:** reclaimable_compose_project, docker compose janitor, port eviction self-heal, cox- prefix, coxagent live hub protected, cox-infra protected, DockerComposeDeploy, deploy reclaim policy

## Overview

CXA-F026 extracts one source of truth — `reclaimable_compose_project()` — for which docker compose projects an automated pass may tear down (`down` / evict). Both the deploy port-eviction self-heal (`DockerComposeDeploy`) and the hourly docker janitor used to carry their own private copies of that policy; drift between them was a standing foot-gun where one side could start treating production as reclaimable and take it down.

The predicate lives once in `crates/infrastructure/src/deploy/reclaimable.rs`. Only agent-managed preview deployments (`cox-…`) are reclaimable; the live hub (`coxagent` plus any prefixed service) and shared backing infra (`cox-infra`) are never reclaimable regardless of case. It is a pure decision function with no IO — callers feed it project names read from Docker.

## How it works

`reclaimable_compose_project(project: &str) -> bool` lowercases its argument once via `to_ascii_lowercase()`, then applies three checks:

1. Return `false` immediately if the lowercased name equals or starts with `cox-infra` or `coxagent` (the live hub plus every service sharing its prefix).
2. Otherwise return whether it starts with `cox-`.
3. Any foreign (non-`cox-`) project returns `false`.

Because case folding happens before comparison, none of those protections can be spoofed by writing e.g. `COXAGENT`, `CoxAgent-Gateway`, or `COX-INFRA`.

Two call sites consume it:

- **Deploy port-eviction** (`crates/infrastructure/src/deploy/docker_compose.rs:1141`, :1211): when an agent's host port is already bound by a stale compose project or container, the deploy attempts up-to-two eviction rounds before retrying. Every candidate runs through this predicate first; if a candidate is not reclaimable (`!reclaimable_compose_project(&project)`), eviction stops and reports a collision instead of ever running `docker compose … down`. The same guard at :1141 refuses to deploy onto a port owned by anything non-reclaimable.
- **Docker janitor** (`crates/presentation/src/server/docs.rs:40`, spawned at server/mod.rs:568): an hourly tick runs `docker compose ls -a --format json`, iterates every listed project name, and skips anything this predicate rejects before deciding whether to issue `docker compose -p <name> down --remove-orphans`. Only fully-stopped projects are reclaimed except stale PR previews still running past PREVIEW_TTL.

Both paths import from re-exports declared in `crates/infrastructure/src/deploy/mod.rs`.

## Usage

No CLI or API surface exists for end users — this is an internal Rust library function consumed by two subsystems at runtime.

```rust
use coxagent_infrastructure::deploy::reclaimable_compose_project;

assert!(reclaimable_compose_project("cox-cxa-codebase"));   // agent preview        -> true
assert!(reclaimable_compose_project("cox--preview-42"));    // stale PR preview     -> true
assert!(reclaimable_compose_project("cox-my-project-preview"));
assert!(!reclaimable_compose_project("coxagent"));           // live hub            -> false
assert!(!reclaimable_compose_project("coxagent-gateway"));   // hub service         -> false
assert!(!reclaimable_compose_project("COXAGENT-GATEWAY"));   // case cannot spoof   -> false
assert!(!reclaimable_compose_project("cox-infra-db"));       // shared infra child  -> false
assert!(!reclaimable_compose_project("someone-elses-stack"));// foreign             -> false
```

Observable behaviours you can trigger on a host:

- Start an agent-driven preview on a free port; confirm later deploys evict only other previews.
- Leave someone else's non-preview stack squatting your host port; confirm deployment reports a collision rather than tearing that stack down.
- Leave stopped-but-not-down preview containers around; watch them disappear on the hourly janitor tick while stopped production containers stay put.

## Interface

Pure Rust function exported twice from this crate:

```
pub fn reclaimable_compose_project(project: &str) -> bool
```

Re-exports so both consumers import from one place:

```rust
// crates/infrastructure/src/deploy/mod.rs
pub use reclaimable::reclaimable_compose_project;
```

Consumers reference it as either crate-internal path (`super::reclaimable::...`) in infrastructure code or public path (`coxagent_infrastructure::deploy::...`) in presentation code.

## Configuration

There is no configuration surface — every protection threshold is a hard-coded literal inside `reclaimable_compose_project()`. The four comparisons that define behaviour, applied to an ASCII-lowercased copy of `project`, are:

```
lower == "cox-infra"
lower == "coxagent"
lower.starts_with("coxagent")
lower.starts_with("cox-infra")
```

followed by `lower.starts_with("cox-")` for everything else. None of these come from env vars, flags, or config files; they cannot be tuned at runtime without editing source and its unit tests together. To extend protection to another namespace you add a literal to this function (and a case-spoof test in its `#[cfg(test)]` module), not a setting.

## Edge cases and limits

What it deliberately does NOT do:

- It never inspects what containers exist inside a project — that is each caller's job (janitor requires all-stopped except stale running previews).
- It treats any non-`cox*` project as foreign and refuses it even if its name otherwise looks harmless.
- It does not itself run any Docker command; it is purely advisory over names supplied by callers.
- Case-insensitivity means protection holds for uppercase/mixed-case spellings but also means you cannot carve out e.g. an all-caps sibling namespace once protected prefixes match after folding.
- A currently-running protected-looking spoof like something named exactly matching another real stack is judged solely by its composed project *name*; Compose already made that distinction upstream via naming rules described elsewhere.

Failure modes are conservative-by-design: returning false protects more than releasing resources prematurely would allow to slip through. There are no panics/props paths here beyond simple slicing logic applied only after prefix length guarantees hold on ASCII-folded input (so no empty-string slicing issues arise).

## Code map

- crates/infrastructure/src/deploy/reclaimable.rs — THE module for CXA-F026: `pub fn reclaimable_compose_project()` plus its `#[cfg(test)]` unit matrix covering previews vs hub vs shared infra vs case-spoof vs foreign projects.
- crates/infrastructure/src/deploy/mod.rs — declares `pub mod reclaimable;` and re-exports `pub use reclaimable::reclaimable_compose_project;` so both consumers import from one place.
- crates/infrastructure/src/deploy/docker_compose.rs — deploy port-eviction self-heal: refuses to deploy onto a non-reclaimable project (`:1141`) and guards every eviction candidate before `compose down` during the up-to-two eviction rounds (`:1211`).
- crates/presentation/src/server/docs.rs — hourly docker janitor (`docker_janitor`, spawned at server/mod.rs:568): iterates `compose ls -a --format json`, skips any project this predicate rejects, else issues `compose -p <name> down --remove-orphans`.

## Related

- docs/wiki/engineering/deployment/cxa-f026-standalone-backend-stack.md — the other half of CXA-F026: the dedicated Compose stack (`cxa-backend`) that runs only the hub's own Postgres/Redis without colliding with CoXAgent's containers.
- docs/wiki/engineering/deployment/cxa-b001-docker-compose-deploy-failure.md — documents the compose service/project-name collision between CoXAgent's own containers and web/split deploys that motivated keeping project naming under our control.
- Coxagent_Ticket_CXA-F026 (git 7dfaed0 "feat(CXA-F026): extract shared reclaimable_compose_project predicate for deploy and janitor (#200)") — landed implementation this page documents.
