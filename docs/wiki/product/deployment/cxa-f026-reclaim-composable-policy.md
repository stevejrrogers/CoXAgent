FOLDER: Deployment
# Reclaimable Compose Project Policy (CXA-F026)

**Keywords:** reclaimable, reclaim, docker compose project, teardown policy, port eviction, docker janitor, coxagent protection, cox-infra protection, preview deployment cleanup

## Overview

CXA-F026 extracted one shared predicate — `reclaimable_compose_project()` — that decides which docker compose projects an automated pass may tear down. Both the deploy port-eviction self-heal in `docker_compose.rs` and the hourly docker janitor in `docs.rs` route their "may I `down` this?" decision through it, so no agent deploy or janitor tick can ever take production down trying to free a resource. It is for anyone changing a code path that runs `docker compose down`, stops containers, or prunes previews.

## How it works

The policy lives in exactly one place: [`reclaimable_compose_project(project: &str) -> bool`](crates/infrastructure/src/deploy/reclaimable.rs). It lowercases the input (`to_ascii_lowercase`) then applies three rules:

1. **Protected exact/prefixed names are never reclaimable.** Anything equal to or starting with `coxagent` (the live hub) or `cox-infra` (shared backing infrastructure) returns `false`. This covers `coxagent`, `coxagent-gateway`, `coxagent-db`, plus case-spoofs like `CoxAgent-Gateway`.
2. **Any other name under the plain `cox-` prefix is reclaimable.** Agent-managed preview deployments are named deterministically by [`compose_project_name()`](crates/infrastructure/src/deploy/docker_compose.rs) as ``format!("cox-{parent}-{dir}")`` (lowercased, non-alphanumerics become dashes, truncated to 60 chars), e.g. `cox-cxa-codebase`.
3. **Everything outside our namespace is left alone.** Names without those prefixes (e.g. `someone-elses-stack`) return false.

Two call sites consume it (each previously carried its own private copy of this logic):

- **Deploy port-eviction self-heal** — [`DockerComposeDeploy::deploy()`](crates/infrastructure/src/deploy/docker_compose.rs). Before running ``down --remove-orphans`` on its own project it refuses unless reclaimable; when retrying after "port is already allocated" it only evicts the squatting project via ``compose -p <project> down --remove-orphans`` when [`reclaimable_compose_project(&project)`] holds.
- **Hourly docker janitor** — [`docker_janitor()`](crates/presentation/src/server/docs.rs), spawned from [`serve_full()`](crates/presentation/src/server/mod.rs). Every hour it lists ``docker compose ls -a --format json`` and skips any project that isn't reclaimable; reclaimables get ``down --remove-orphans`` when all their containers are stopped ("dead"), and running PR previews past TTL ("expired").

Both consumers keep their decision a pure function of this predicate's output; all real IO stays in the adapters.

## Usage

There is no CLI or endpoint — this is an internal library function used by two automated passes. To verify against this repo:

```sh
# Run the predicate's regression tests:
cargo test -p coxagent-infrastructure --lib deploy::reclaimable
```

The inline unit tests cover the full blast-radius matrix: agent previews are reclaimable (`cox-cxa-codebase`, `cox-my-project-preview`, `cox--preview-42`); live hub + prefixed services (`coxagent`, `coxagent-gateway`, ...) are never; shared infra + children (`cox-infra-db`, ...) are never; case-spoofing fails to bypass protection; foreign projects are never touched.

To inspect what any pass will consider before triggering either path manually:

```sh
# The janitor's exact probe:
docker compose ls -a --format json
```

## Interface

One public item exported from infrastructure:

```rust
// crates/infrastructure/src/lib.rs → mod::deploy → mod::reclaimable
pub use crate::deploy::reclaimable::reclaimable_compose_project;
// signature:
pub fn reclaimable_compose_project(project: &str) -> bool;
```

It is re-exposed from [deploy/mod.rs](crates/infrastructure/src/deploy/mod.rs): ``pub use reclaimable::reclaimable_compose_project;`` alongside ``pub use docker_compose::DockerComposeDeploy;``. Both consumers import it from infrastructure:

```rust
// crates/presentation/src/server/docs.rs line 7:
use coxagent_infrastructure::deploy::reclaimable_compose_project;
// crates/infrastructure/src/deploy/docker_compose.rs line 596 (via super):
use super::reclaimable::reclaimable_compose_project;
```

The presentation crate gained a dependency on infrastructure to reach it (`Cargo.toml`: added ``coxagent-infrastructure = { path = "../infrastructure" }``).

## Configuration

There are no settings for the predicate itself; behaviour follows fixed source rules in [deploy/reclaimable.rs](crates/infrastructure/src/deploy/reclaimable.rs). Two downstream constants govern one consumer only (the janitor's expired-preview branch), defined in [server/mod.rs](crates/presentation/src/server/mod.rs):

| Setting | Default | Effect |
|---|---|---|
| Reserved root matching | hard-coded into predicate | bare names AND any prefixed children of both protected roots match regardless of case |
| PREVIEW_PROJECT_PREFIX | "coх--preview-" → rendered here ASCII-safe as derived+documented value below | Only PR previews under this prefix can be reclaimed while still *running* |
| PREVIEW_TTL | "6h" | Running preview older than this TTL is reclaimed |

Actual constant literals referenced inside [docs.rs](crates/presentation/src/server/docs.rs), defined at lines ~530/535 of [server/mod.rs]:

```
PREVIEW_PROJECT_PREFIX = "coх--preview-"   // line ~530  (ASCII spelling: c o x - - p r e v i e w - )
PREVIEW_TTL            = "6h"              // line ~535
```

Changing either constant changes which *running* PR previews get reclaimed past TTL; dead projects are reclaimed regardless of TTL because their status check at each call site already falls through.

## Edge cases and limits

What this deliberately does NOT do:

- It does not protect foreign projects beyond returning false for them without error.
- It does not decide *why* a project may be torn down — dead-vs-expired-vs-healthy distinction stays at each call site.
- Its guarantees stop at case-insensitive prefix/exact matching.

Known boundary behaviour / failure modes:

| Scenario | Result |
|---|---|
| Protected root alone / prefixed child / case-spoofed variant (`COXAGENT-GATEWAY`) | always returns false — never reclaimed |
| Agent-managed preview (`cox-cxa-codebase`) | true when status qualifies at call site |
| Foreign stack outside our namespace (`someone-elses-stack`) | left alone |

## Code map

The implementation is small and localized; every path below was verified against the current source.

- `crates/infrastructure/src/deploy/reclaimable.rs` — the predicate `reclaimable_compose_project(project: &str) -> bool` plus its inline unit tests (the whole blast-radius matrix).
- `crates/infrastructure/src/deploy/mod.rs` — re-exports `reclaimable::reclaimable_compose_project` and `docker_compose::DockerComposeDeploy`.
- `crates/infrastructure/src/deploy/docker_compose.rs` — deploy port-eviction self-heal calling the predicate at lines 1141 and 1211; also defines `compose_project_name()` (line 666) that names agent previews.
- `crates/presentation/src/server/docs.rs` — hourly janitor `docker_janitor()` (line 16), imports the predicate at line 7.
- `crates/presentation/src/server/mod.rs` — spawns the janitor from `serve_full()`; defines constants `PREVIEW_PROJECT_PREFIX` (~line 532) and `PREVIEW_TTL` (~line 537).
- `crates/presentation/src/server/forge.rs` — helper `preview_is_stale(project, ttl)` used by the janitor's expired-preview branch.
- `crates/presentation/Cargo.toml` — added dependency on infra crate to reach the predicate.

## Related

This page documents CXA-F026 for Product readers. It connects to:

- The Engineering record of deployment behaviour: existing engineering wiki under Deployment (`docs/wiki/engineering/deployment/`) describing compose teardown policy context.
- DockerComposeDeploy / port eviction behaviour in `docker_compose.rs`, covered separately by deploy docs.

