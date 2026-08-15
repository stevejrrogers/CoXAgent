FOLDER: Walking Skeleton

# Tony Language — Project Activation & Walking Skeleton

**Keywords:** Tony Language, TL, tony-language, walking skeleton, onboarding, cycle 0, brownfield adoption, project_context.md, host port 8101, metrics-server /health

## Overview

CXA-F003 is CoXAgent's first real product-project activation: it adopted the existing Tony Language ("TL") compiler codebase as a managed project under `deploy/data/TL` and delivered its walking skeleton — the minimal end-to-end slice that proves build → test → containerize → health-probe before any larger feature starts (PLAN §3g "cycle 0", step 7). It is for anyone who needs to know how a brand-new product gets onboarded into CoXAgent and what its first shipped artifact looks like.

## How it works

Activation follows PLAN §3g Flow B (brownfield adoption) of an existing codebase:

1. The runner adopts TL via the brownfield scaffolder in [`crates/app/src/onboard.rs`](crates/app/src/onboard.rs), which indexes the codebase into `.coxagent/REPO_MAP.md`, detects stack (`detect_stack`) and docker (`analyze_docker`), then auto-drafts an initial `project_context.md`.
2. During this activation cycle CXA-F003 replaces that blank template with real product context in [`deploy/data/TL/state/project_context.md`](deploy/data/TL/state/project_context.md): Rust workspace with one crate per compiler stage (`tl-ast` → `tl-lexer`/`tl-parser` → `tl-hir` → `tl-typecheck` → `tl-mir` → `tl-interpreter`) plus `tl-driver` as the CLI binary; dockerized deploy; product definition (a systems language treating distributed data as first-class); MVP scope; and conventions.
3. Two tickets are seeded into [`deploy/data/TL/state/state.json`](deploy/data/TL/state/state.json):
   - **TL-F002** "Walking skeleton: hello-world service with /health endpoint", pointing at `examples/walking-skeleton.tn`, tests in `crates/tl-driver/tests/walking_skeleton.rs`, compose app on host port 8101.
   - **TL-B003** "[Baseline] Failing unit test router_migrate_actor_preserves_state_and_flushes_buffered" — a pre-existing failure at `crates/tl-runtime/src/router.rs:325`, explicitly not introduced by this work.

The walking-skeleton program itself is deterministic so tests assert exact output ([`examples/walking-skeleton.tn`](codebase/examples/walking-skeleton.tn)), while a long-running companion ([`examples/walking-skeleton-server.tn`](codebase/examples/walking-skeleton-server.tn)) keeps the process alive so containers answer health probes.

## Usage

### Verify the pipeline locally (in the TL codebase)

```sh
cd /Users/luton/Projects/TonyLanguage
tl run examples/walking-skeleton.tn        # deterministic demo output
cargo test -p tl-driver                    # runs tests/walking_skeleton.rs e2e suite
```

Diagnose individual stages:

```sh
tl lex       examples/walking-skeleton.tn
tl parse     examples/walking-skeleton.tn
tl typecheck examples/walking-skeleton.tn  # -> "typecheck: ok"
```

### Run as a service and probe health

```sh
docker compose up --build -d app           # publishes host 8101 -> container :8080
curl http://localhost:8101/health          # {"healthy":true,"checks":[{"name":"process_alive","ok":true}]}
docker compose down                        # stop when done
```

### Inspect onboarded state

```sh
coxagent report --state-dir deploy/data/TL   # or read deploy/data/TL/{project_context.md,tickets}
```

## Interface

CXA-F003 adds no new network/REST interface beyond what onboarding already provides. The surfaced interfaces are:

### Health endpoints served by tl-runtime metrics server (`crates/tl-runtime/src/metrics_server.rs`)
Bound when the driver receives `--metrics-addr <addr>`:

| Path | Returns |
|------|---------|
| GET `/health` | liveness JSON — HTTP 200 + healthy when up |
| GET `/healthz` | same liveness JSON as `/health` |
| GET `/readyz` | readiness JSON (declared checks) |
| GET `/metrics` | Prometheus text format |

### docker-compose service mapping (`codebase/docker-compose.yml`)
```yaml
services:
  app:
    build: { context: ".", dockerfile: Dockerfile.builder }
    command:
      - run
      - --metrics-addr=0.0.0.0:8080
      - examples/walking-skeleton-server.tn
    ports:
      - "8101:8080"     # host -> container; do not rebind elsewhere
```

## Configuration

Activation needed no manual configuration — defaults sufficed and generated artifacts were edited afterwards:

- No activation-specific flags beyond onboarding's standard greenfield/brownfield options.
- Host port selection comes from onboarding's reserved-port logic; here pinned to **8101** in compose.
- Engine defaults written during adoption come from whatever opencode/claude detection produced for this workspace's coxagent.json.
- Conventions (lint gates) recorded in the project context's "Conventions to respect" section guide later runs.

No new config keys were introduced by CXA-F003; behaviour is defined entirely by seeded state plus committed context/compose files.

## Edge cases and limits

This page documents what CXA-F003 delivered; it does not re-explain generic onboarding behaviour (see Related):

- It ships only activation artifacts (refined context + two seeded tickets); actual walking-skeleton *delivery* on top of those seats is tracked by TL-F002 / TL-B003 tickets rather than completed here.
- The working tree you check out today may show the *pre-refinement* auto-drafted context because this ticket lives on its own branch `feat/CXA-F003`, not yet merged into other active branches.
- Host port 8101 is reserved for this project in compose; do not rebind it elsewhere.
- The commit also accidentally included a stray junk file `lmp` (captured `lsof` usage output) at the repo root — debris, not part of activation, that should be removed from any future merge of this branch.
- CXA-F003 documents only activation on CoXAgent's side (`deploy/data/TL`); all TL-side walking-skeleton source lives in `/Users/luton/Projects/TonyLanguage`, which is outside this worktree (symlinked as `codebase`).

## Code map

Files implementing / produced by CXA-F003:

- deploy/data/TL/state/project_context.md — refined TL product context (stack, deploy, MVP scope, success criteria) written during this cycle.
- deploy/data/TL/state/state.json — seeded tickets TL-F002 (walking skeleton) and TL-B003 (baseline bug).
- codebase/examples/walking-skeleton.tn — deterministic pipeline demo program shipped by the ticket (in the adopted TL codebase).
- codebase/examples/walking-skeleton-server.tn — long-running companion serving health probes via `--metrics-addr`.
- codebase/crates/tl-driver/tests/walking_skeleton.rs — e2e suite driving each pipeline stage against the real binary.
- codebase/Dockerfile.builder — builds `tl-driver` into a slim debian image shipping the walking-skeleton server.
- codebase/docker-compose.yml — runs the app container publishing host 8101 → container :8080 with `/health`.
- crates/app/src/onboard.rs — brownfield/greenfield scaffolders that produced the adopted workspace.

## Related

Existing wiki page [`wiki/Product/project-activation.md`](project-activation.md) documents the general onboarding mechanism (`greenfield()` / `brownfield()`, human gate) that CXA-F003 exercised; this page documents one concrete instance of it. PLAN §3g ("cycle 0", steps: walking skeleton = FEAT seeding, baseline TEST for brownfield Flow B) defines why this slice exists. Ticket numbers TL-F002 / TL-B003 track follow-on delivery and baseline repair respectively.
