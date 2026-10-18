FOLDER: Engineering

# Shared Reclaimable-Compose-Project Predicate

**Keywords:** reclaimable, compose project, docker janitor, port eviction, self-heal, live hub, cox-infra, blast radius, teardown policy, deploy collision

## Overview

CXA-F026 removes a standing foot-gun in CoXAgent's docker lifecycle: two unrelated components — the deploy port-eviction self-heal and the hourly docker janitor — each decided *for themselves* which compose projects they were allowed to tear down. The deploy side checked `evictable_project`, the janitor checked `name.starts_with("cox-") || name == "cox-infra"`. If those policies ever drifted — e.g. one side started treating the live hub or shared backing infra as reclaimable — an agent deploy or a janitor tick could take production down trying to free resources it thought it owned.

The fix extracts that decision into a single pure function, `reclaimable_compose_project()`, living in its own module (`crates/infrastructure/src/deploy/reclaimable.rs`), and routes **both** call sites through it so they can never drift again. It is for anyone who maintains deploy teardown or container-cleanup code: there is now exactly one place that answers "is this project safe to `down`?"

## How it works

The predicate is a pure decision over one string (the compose project name), so both consumers share identical semantics:

```
reclaimable_compose_project(project) -> bool
```

1. Normalise to lowercase (`to_ascii_lowercase`) so protection cannot be spoofed by casing (`COXAGENT`, `CoxAgent-Gateway`, `COX-INFRA` all resolve to protected).
2. Refuse anything that is the live hub (`coxagent`), any service container sharing its prefix (`starts_with("coxagent")`), shared backing infra (`cox-infra`), or any of its children (`starts_with("cox-infra")`) — tearing these down is a self-inflicted control-plane outage.
3. Otherwise reclaim only agent-managed preview projects recognisable by their `cox-` prefix; everything else (foreign/non-preview stacks) is left alone.

The two consumers call it in place of their old inline logic:

- **Deploy port-eviction** in [`crates/infrastructure/src/deploy/docker_compose.rs`](crates/infrastructure/src/deploy/docker_compose.rs) refuses to start a project that would collide with a non-reclaimable project sitting on an agent host port ("refusing to deploy project … collides with the live hub"); during eviction of squatting projects it skips anything not reclaimable.
- **Docker janitor** ([`docker_janitor()`](crates/presentation/src/server/docs.rs)) replaces its ad-hoc filter with a single `if !reclaimable_compose_project(name) { continue; }`.

To reach both crates cleanly F026 wires up re-exports: [`crates/infrastructure/src/deploy/mod.rs`](crates/infrastructure/src/deploy/mod.rs) declares `pub mod reclaimable;` and re-exports `pub use reclaimable::reclaimable_compose_project;`. The presentation crate adds a dependency on `coxagent-infrastructure`, then imports the symbol directly for its janitor task.

## Usage

There are no runtime knobs for operators — F026 changes internal policy wiring only. The relevant usage is at build time and for future maintainers who touch teardown code.

Calling the predicate from Rust:

```rust
use coxagent_infrastructure::deploy::reclaimable_compose_project;

// Agent-managed preview -> true
assert!(reclaimable_compose_project("cox-cxa-codebase"));
assert!(reclaimable_compose_project("COX--preview-42"));

// Live hub / shared infra / foreign stacks -> false
assert!(!reclaimable_compose_project("coxagent"));
assert!(!reclaimable_compose_project("COXAGENT-GATEWAY"));
assert!(!reclaimable_compose_project("cox-infra-db"));
assert!(!reclaimable_compose_project("someone-elses-stack"));
```

Verify end-to-end after touching this code path:

```bash
cargo check --workspace && cargo clippy --workspace --all-targets && cargo fmt --check
```

Behaviour observable after this change: deploying against an occupied port colliding with your own live hub still errors with "refusing to deploy project … collides with the live hub", exactly as before — only the *source* of that decision moved.

## Interface

Single public function:

| Symbol | Signature | Meaning |
|---|---|---|
| [`reclaimable::reclaimable_compose_project(&str)`](crates/infrastructure/src/deploy/reclaimable.rs) | `#[must_use] fn(&str) -> bool` | True iff an automated pass may tear down / evict this compose project |

Re-exports:
- [`deploy/mod.rs`: pub mod + pub use](crates/infrastructure/src/deploy/mod.rs)
- Consumed by presentation via `use coxagent_infrastructure::deploy::reclaimable_compose_project;`

Replaced inline predicates (removed):
- private [`evictable_project(&str)`](crates/infrastructure/src/deploy/docker_compose.rs) (port-eviction blast-radius guard)
- ad-hoc janitor filter in [`docker_janitor()`](crates/presentation/src/server/docs.rs)

No HTTP endpoints, CLI flags, config keys, or environment variables were added or changed by CXA-F026.

## Configuration

None. CXA-F026 adds no configuration surface — behaviour is determined entirely by hard-coded protected prefixes inside `reclaimable_compose_project()`. If you need some new namespace reclaim-protected later, edit that function and both consumers follow automatically.

## Edge cases and limits

What F026 deliberately does **not** do:
- It does not decide *when* teardown happens (that stays per-consumer: deployed-port collision vs hourly stopped/expired-project sweep). It only answers *whether* a given project may be reclaimed.
- It does not reason about Docker status at all (running vs stopped); deciding whether something running qualifies as a stale preview remains each caller's job.
- Protection covers exactly our reserved names/prefixes (`coxagent*`, `cox-infra*`) case-insensitively plus generic foreign exclusion; nothing else gets special treatment.

The exact decision matrix lives as unit tests in [`reclaimable.rs`](crates/infrastructure/src/deploy/reclaimable.rs): five groups covering agent previews → true; live hub + prefixed services → false; shared infra + children → false; case-spoofing of protected names → false; foreign/non-preview projects → false.

Failure modes if regressed:
- If a future teardown path bypasses this predicate, drift returns silently until one side tears down the wrong thing — exactly what F026 exists to prevent.
- No fail-open path exists here because both call sites already skip on `false` rather than error out (janitor continues; deploy reports a collision error).

## Code map

- `crates/infrastructure/src/deploy/reclaimable.rs` — new module: single source of truth for reclaimability policy (`reclaimable_compose_project`) plus its five unit-test groups
- `crates/infrastructure/src/deploy/mod.rs` — declares `pub mod reclaimable;`, re-exports `reclaimable_compose_project`
- `crates/infrastructure/src/deploy/docker_compose.rs` — deploy port-eviction now calls `reclaimable_compose_project` in place of private `evictable_project` (removed); old inline test replaced by reference to shared module's tests
- `crates/presentation/Cargo.toml` — adds dependency on `coxagent-infrastructure`
- `crates/presentation/src/server/docs.rs` — janitor task uses `reclaimable_compose_project(name)` instead of ad-hoc prefix/name checks

Note: this worktree snapshot predates the F026 merge commit (`ef99b2e / 9023969 "feat(CXA-F026)"`), so the working tree here still shows pre-F026 state (`evictable_project`, ad-hoc janitor filter). The paths above are where each piece lands under that commit.

## Related

- [COX-F005.md](COX-F005.md) — pre-deploy health gate and port-collision behaviour that the eviction guard protects
- [CXA-B004-config-initializer.md](CXA-B004-config-initializer.md) — deploy config plumbing (`deploy.host_port`) that determines which ports can collide
- ADAPTIVE_APPROVAL.md / HYBRID_TEAM.md — board/ticket flows unrelated to docker teardown policy but part of the same docs set
