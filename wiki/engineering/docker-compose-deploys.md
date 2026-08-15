FOLDER: Deployment

# Docker Compose Deploys & Deploy Failures (CXA-B001)

**Keywords:** docker compose, deploy failed, build details, Docker Desktop, dead_code warnings deny, health gate, rollback, LAST_GOOD_REF, DockerComposeDeploy, host_port

## Overview

This page documents how an agent-driven project gets deployed with `docker compose up -d --build`, how a failed build or boot surfaces as a ticket titled "Deploy failing: docker compose failed: View build details: docker-desktop://dashboard/build/…", and what actually fixes that class of failure. It is for anyone who sees a CXA-B001-style ticket or works on the deploy adapter (`DockerComposeDeploy`) or its post-deploy health gate.

CXA-B001 was one of these tickets: a deploy whose builder step failed so no image was produced. Its root cause is recorded in commit `2e415f7` — this repo's own standalone backend stack had no dedicated compose file and collided with the web-app compose project on the same host; that commit adds `deploy/docker-compose.cxa.yml` under its own project (`cxa-backend`) so CXA deployments stop stepping on each other.

## How it works

The full lifecycle lives in three layers:

1. **Adapter** — `DockerComposeDeploy::deploy(work_dir)` in crates/infrastructure/src/deploy/docker_compose.rs runs `docker compose -p <proj> up -d --build` (timeout 900s). On failure it reports a summary literally formed as `"docker compose failed: {detail}"` where `{detail}` is the last non-empty stderr line from Docker Desktop — often exactly the build-details URL you see in the ticket title (line 1299).
2. **Health gate** — after a successful exit it polls `verify_deploy_health()` / `run_health_check()` against `config.deploy.host_port`, because "compose exits 0" only proves containers started. A container that never binds its port is downgraded to failure (`describe_health_failure`).
3. **Cycle wiring** — crates/application/src/use_cases/cycle/mod.rs (`RunCycleUseCase::execute`) calls `deploy.deploy()`, then if it ran but isn't healthy calls `record_deploy(false)`, files a bug via `file_deploy_bug()`, and finally triggers auto-rollback via `attempt_rollback()` when configured.

The exact ticket format comes from two pieces:

- `DockerComposeDeploy::deploy()` builds the summary `"docker compose failed: {detail}"` where `{detail}` is the last non-empty stderr line (crates/infrastructure/src/deploy/docker_compose.rs, around the failure branch).
- `file_deploy_bug()` in crates/application/src/use_cases/cycle/ops.rs prepends its constant `MARKER = "Deploy failing"` to that summary to form the ticket title, and dedupes against any open bug already starting with that marker.

So `Deploy failing: docker compose failed: View build details: …` *is* CXA-B001-shaped output — every such ticket means "the actual fix must make `docker compose up -d --build` succeed". Port clashes get special-cased in `file_deploy_bug`: a bind/allocation error in the summary produces acceptance criteria telling the agent to map ports from env vars instead of hardcoding, while anything else just asks for root-cause + fix.

Rollback (optional): when ``config.deploy.auto_rollback`` is true and the forward deploy fails but a fresh-enough ``LAST_GOOD_REF`` exists (and no migration shipped since), ``attempt_rollback`` redeploys that good sha into a dedicated `<name>-rollback` worktree and re-runs the same health gate.

## Usage

There is no user-facing command; deploys run automatically each cycle after DEV ships work into a project with both:

- A recognized compose file at `<project>/docker-compose.yml|yaml|compose.yml|yaml`.
- ``config.deploy.enabled = true`` (default true) in `<project>/coxagent.json`.

If you are an operator debugging one of these tickets by hand:

```sh
# Reproduce exactly what the adapter runs:
cd <project-codebase>
PG_PASSWORD=<your-pg-secret> COXAGENT_ADMIN_PASSWORD=<your-admin-secret> \
  docker compose -p cox-<parent>-<dir> up -d --build

# See why bootstrap failed after start:
docker ps -a
docker logs <container>

# Confirm what made it fail at BUILD time:
docker system prune -af        # clear builder cache for a clean rebuild
```

Root-cause checklist that has resolved prior tickets of this shape:

1. `${VAR:?}` interpolation missing PG_PASSWORD / COXAGENT_ADMIN_PASSWORD → seeded automatically by resolution; check your own env isn't clobbered.
2. Linux-only dead code compiled under `warnings = "deny"` killing the builder → see platform_gates below.
3. A published host port already allocated by another stack → port-eviction self-heals only evictable (`cox-*`) preview projects; live hub/infra are protected.
4. Compose projects named identically colliding on one host → separate them under their own project names / volumes.

## Interface

Trait boundary (crates/application/src/ports/outbound/deploy.rs):

- trait DeployPort — async fn deploy(&self, work_dir) -> Result<DeployReport>, async fn ensure_daemon(), async fn down(), async fn health(port), async fn health_check(port) -> HealthCheckResult, async fn wait_healthy(port, timeout), async fn lint[_report](), cross_target_check(), run_tests_scoped(), run_tests()
- struct DeployReport { success: bool; deployed: bool; summary: String }
- struct CrossCheck { available; reason; errors } — used by cross_target_check
- pub fn verify_deploy_health(deploy,&host_port) -> bool — shared mandatory poll (15×2s)
- pub fn parse_deploy_host_port(raw_config) -> Result<Option<u16>, ()>
- pub const fn is_publishable_host_port(port) -> bool — rejects 0

Adapter implementation (crates/infrastructure/src/deploy/docker_compose.rs):

- struct DockerComposeDeploy — impl DeployPort
- const COMPOSE_FILES = ["docker-compose.yml","docker-compose.yaml","compose.yml","compose.yaml"]
- const DEPLOY_TIMEOUT = 900s
- const REQUIRED_SECRET_KEYS = ["PG_PASSWORD","COXAGENT_ADMIN_PASSWORD"]
- fn resolve_deploy_secrets[_in](work_dir[, secret_root]) -> Vec<(String,String)> + seed_deploy_secrets()
- fn random_secret(), missing_required_secrets(), read_dot_env()
- fn store_file() / cxb031_store_file() + adopt/persist helpers for durable fallbacks
- helper fns extract_bind_port(), compose_project_on_port(), evictable_project(), apply_resource_limits(), running_services()

Cycle + ops wiring (crates/application/src):

- `RunCycleUseCase<S,E>::execute` — invokes `deploy.deploy()`, runs `run_health_check()` / `verify_health_after_deploy()`, calls `record_deploy()` / `record_known_good()` / `attempt_rollback()`.
- mod.rs helper fns — `describe_health_failure(result) -> String`, `short_sha(sha)`.
- ops.rs helpers — `record_deploy(ok,summary,sha,health)`, `run_health_check() -> (bool, Option<HealthCheckResult>)`, `verify_health_after_deploy()`, `record_known_good(sha,summary)` (writes LAST_GOOD_REF), `attempt_rollback(reason,failed_sha,&report)`, rollback sub-helpers (`migration_shipped_since`, `finish_rollback`, `file_deploy_bug`).
- const MARKER = "Deploy failing" in ops.rs:403 — the title prefix for every deploy-failure ticket.

## Configuration

All under per-project `<project>/coxagent.json` → section **deploy** (struct DeployConfig in crates/application/src/config.rs):

| Key | Default | Effect |
|---|---|---|
| host_port | None (auto-assigned at onboard; base 8101 range via crates/app/src/host_port.rs) | The published port the health gate probes; 0 is rejected as unpublishable |
| enabled | true | Whether the cycle deploys at all; off for self-hosting projects whose compose file binds the live hub's port |
| auto_rollback | false | Redeploy last known-good on deploy/test failure |
| max_rollback_age_secs | 3600 | Known-good older than this is skipped as stale |
| migration_detection_paths | ["migrations"] | If any file under these prefixes changed since known-good, rollback is skipped (schema-ahead-of-code risk) |
| health_check_timeout_secs | 60 | How long wait_healthy polls before marking a deploy failed |

Adapter-level env overrides (crates/infrastructure/src/deploy/docker_compose.rs):

- COXAGENT_DEPLOY_SECRETS_DIR — root for the out-of-tree fallback-secret store (tests/sandboxes point this at an isolated dir)
- COXAGENT_DEPLOY_CPUS (default "2"), COXAGENT_DEPLOY_MEM (default "1g") — resource clamp applied via `docker update` to every container of a just-deployed project; either set to `off` to skip
- The heavy-slot semaphore width that serializes compose builds is governed by COXAGENT_MAX_PARALLEL_HEAVY in crates/infrastructure/src/proc.rs

## Edge cases and limits

What this deliberately does **not** do:

- It never proves more than "the app bound its port": a non-5xx HTTP answer at `/` counts as healthy even if it has no root route — demanding a 2xx would roll back every valid app without one (`describe` treats 5xx as broken).
- Port-eviction only ever downs agent preview projects (`cox-…`). The live hub (`coxagent*`) and shared infra (`cox-infra*`) are protected by construction.
- No compose file ⇒ deploy is skipped silently (`deployed=false`), not an error.
- Auto-rollback caps at one retry; a second failure escalates via bug+notify instead of looping. First-ever deploy has nothing to roll back to.
- A malformed/non-publishable host_port fails the gate rather than being treated as unconfigured.

Known limits / how it fails:

- A build-time failure produces only Docker Desktop's last stderr line in the summary; deep diagnosis needs you to click into that build-details URL or run compose by hand with a clean builder cache.
- If both required secrets resolve externally nothing is seeded — correct, but means no durable fallback store entry is written either.

## Code map

- crates/infrastructure/src/deploy/docker_compose.rs — `DockerComposeDeploy` (the adapter): deploy/down/lint/tests/cross-target-check, compose project naming, secret resolution + durable store, port-eviction self-heal, resource limits. The source of the literal "docker compose failed: …" summary.
- crates/application/src/ports/outbound/deploy.rs — `DeployPort` trait, `DeployReport`, `CrossCheck`, `verify_deploy_health`, `parse_deploy_host_port`, `is_publishable_host_port`.
- crates/application/src/config.rs — struct DeployConfig and every default (host_port/enabled/auto_rollback/max_rollback_age_secs/migration_detection_paths/health_check_timeout_secs).
- crates/app/src/host_port.rs — host-port assignment at onboarding (assign_host_port) and the published-port registry.
- crates/app/src/config_load.rs / config_parse.rs — heal_host_port at load; refuses out-of-range host_port as a load error (names the field).
- crates/application/src/use_cases/cycle/mod.rs — execute()'s deploy block: calls deploy(), health gate, record_deploy / record_known_good; describe_health_failure(), short_sha().
- crates/application/src/use_cases/cycle/ops.rs — record_deploy / verify_health_after_deploy / run_health_check / attempt_rollback / finish_rollback / file_deploy_bug (`MARKER = "Deploy failing"`).
- crates/app/tests/deploy_smoke.rs — COX-B008 regression smoke test: boots this repo's own compose stack on 8101 and probes 200.
- crates/app/tests/platform_gates.rs — COX-B006/B008 source-gate guarding against Linux-only dead code under warnings=deny killing the builder.
- crates/app/tests/docs_ports.rs + docs_gate tests in run_docs.rs — verify README/compose agree on the published host port.
- deploy/docker-compose.cxa.yml — CXA's standalone Postgres+Redis backend stack (project cxa-backend), added by commit 2e415f7 to resolve CXA-B001's compose collision.

## Related

- wiki Engineering → Deployment → deployment.md (CXA-B017) — the honour-or-generate fallback secrets behind `${VAR:?}` seeding that keeps deploys from dying at interpolation; later durability/permission tickets (B028/B031/B032/B033/B036/B038/B039/B040/B043/B044) are documented there.
- DEPLOYMENT.md at repo root — how this project itself is deployed (macOS app, docker-compose self-host, Helm/k8s).
- CXA-B010 / COX-C012 / COX-C016 — env-interpolation / compose-build reporting fixes on this same adapter.
- COX-B004 / COX-F005 / CXA-B025 / B026 / B042 / B053 — the mandatory post-deploy health gate and its host_port parsing/bounds rules.
- COX-B006 / COX-B008 — the Linux dead-code-under-warnings=deny root cause class that first surfaced as "Deploy failing"; guarded by platform_gates.rs + deploy_smoke.rs.
