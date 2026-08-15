FOLDER: Deployment

# Deploy Failing: docker compose build failure (CXA-B001)

**Keywords:** docker compose, deploy failing, build details, Docker Desktop, View build details, builder step, image not produced, port collision, cxa-backend project, DockerComposeDeploy, health gate

## Overview

CXA-B001 is a deploy-failure bug whose symptom is a ticket titled `Deploy failing: docker compose failed: View build details: docker-desktop://dashboard/build/default/default/<id>`. It occurs when an agent-driven `docker compose up -d --build` fails at the builder step, so no image is produced and nothing starts. This page documents how that exact ticket text is generated end-to-end and what fixed this instance — for anyone debugging a "Deploy failing" bug or working on the deploy adapter (`DockerComposeDeploy`).

Root cause recorded here (commit `2e415f7`): this repo's own standalone backend stack had no dedicated compose file and collided with the web-app compose project on the same host; that commit adds `deploy/docker-compose.cxa.yml` under its own project (`cxa-backend`) so CXA deployments stop stepping on each other's named volumes/projects.

## How it works

The lifecycle spans three layers.

**1. Adapter.** `DockerComposeDeploy::deploy(work_dir)` in `crates/infrastructure/src/deploy/docker_compose.rs` runs `/usr/bin/docker compose -p <proj> up -d --build` under a 900-second timeout (`DEPLOY_TIMEOUT`). On failure it builds its summary as literally `format!("docker compose failed: {detail}")` (line 1299), where `{detail}` is the last non-empty stderr line picked by `last_meaningful()`. For Docker Desktop builds that line is often exactly the "View build details: docker-desktop://..." URL seen in these ticket titles; a genuine compiler error surfaces its last meaningful compiler line instead.

**2. Health gate.** After a successful exit the cycle polls `run_health_check()`, which wraps the shared gate `verify_deploy_health()` against the project's configured published port, held in `config.deploy.host_port`. A green exit only proves containers started; an app that never binds its port is downgraded to failure via `describe_health_failure()`. The B001 shape never reaches this layer — it fails during the build at step 1.

**3. Cycle wiring.** In `RunCycleUseCase::execute` (`crates/application/src/use_cases/cycle/mod.rs`, around line 936) the cycle calls `deploy.deploy(&self.work_dir)`. When the deploy runs but is not healthy it calls `record_deploy(false, &summary, attempt_sha, health_check)`, notifies with kind `deploy_failed`, then files a bug through `file_deploy_bug()`. If deploy or tests failed it drives auto-rollback via `attempt_rollback(reason, attempt_sha, &mut report)`; otherwise it records the sha as known-good via `record_known_good()`.

The exact ticket title comes from two pieces: the adapter summary above (`docker compose failed: {detail}`) and `file_deploy_bug(summary)` in `crates/application/src/use_cases/cycle/ops.rs`, whose constant is `MARKER = "Deploy failing"`. It forms the title as `format!("{MARKER}: {summary}")` and dedupes against any already-open Bug whose title starts with that marker. So every such ticket means "make this repo's stack boot without colliding / make the builder succeed."

Port clashes get special-cased inside ops.rs: if the summary contains `already allocated`, `address already in use`, or `bind for`, acceptance criteria are generated telling an agent to map published ports from an env var defaulting to `config.deploy.host_port` instead of hardcoding them; otherwise criteria stay empty and only root-cause-and-fix is requested.

## Usage

There is no user-facing command; deploys run automatically each cycle once DEV ships work into a project that has both:

- a recognized compose file at `<project>/docker-compose.yml|yaml|compose.yml|yaml`, and
- `config.deploy.enabled = true` (the default) in `<project>/coxagent.json`.

Reproducing what the adapter runs by hand:

```sh
cd <project-codebase>
PG_PASSWORD=... COXAGENT_ADMIN_PASSWORD=... \
  docker compose -p cox-<parent>-<dir> up -d --build

# see why bootstrap failed after start
docker ps -a
docker logs <container>

# force a clean rebuild to rule out builder-cache noise
docker system prune -af
```

Root-cause checklist that has cleared prior tickets of this shape:
1. `${VAR:?}` interpolation missing PG_PASSWORD / COXAGENT_ADMIN_PASSWORD — auto-seeded by resolution; check your own env isn't being clobbered.
2. Linux-only dead code compiled under deny-warnings killing the builder — see platform gates.
3. A published host port already allocated by another stack — port-eviction self-heals only evictable (`cox-*`) preview projects; live hub/infra are protected.
4. Compose projects named identically colliding on one host — separate them under distinct project names/volumes (**the B001 fix**: give the stack its own `.yml` under its own project).

## Interface

| Symbol | Location | Role |
|---|---|---|
| `DockerComposeDeploy::deploy` | crates/infrastructure/src/deploy/docker_compose.rs | runs `/usr/bin/docker compose -p <proj> up -d --build`, seeds secrets |
| `compose_project_name(work_dir)` | same file | deterministic project name `cox-<parent>-<dir>` (sanitized) |
| `evictable_project(proj)` | same file | blast-radius guard for port-eviction retry |
| `resolve_deploy_secrets(work_dir)` / `seed_deploy_secrets` | same file | honour env/dotenv else random fallback for required vars |
| `verify_deploy_health(deploy, host_port)` | crates/application/src/ports/outbound/deploy.rs | shared mandatory post-deploy probe loop |
| `parse_deploy_host_port(raw_config)` / `is_publishable_host_port(p)` | same file | validate/normalize a publishable host port |
| `RunCycleUseCase::execute` (~line 936) + health gate wiring ~957..1048 | crates/application/src/use_cases/cycle/mod.rs | calls deploy, gates health, files bugs, decides rollback |
| `file_deploy_bug(summary)`, MARKER = "Deploy failing", title = `format!("{MARKER}: {summary}")` | crates/application/src/use_cases/cycle/ops.rs (~401) | mints/dedupes the high-priority Bug ticket |

## Configuration

All under the project's `deploy` section in `<project>/coxagent.json`, deserialized into `DeployConfig` (`crates/application/src/config.rs`) with these defaults:

| Key | Default | Effect |
|---|---|---|
| deploy.host_port | None | host port to publish / health gate to probe; assigned per project at onboard |
| deploy.enabled | true | whether the cycle deploys at all |
| deploy.auto_rollback | false | auto-redeploy last known-good on a failed deploy/tests |
| deploy.max_rollback_age_secs | 3600 | older known-good is too stale to roll back to |
| deploy.migration_detection_paths | ["migrations"] | prefixes marking DB migrations; change since known-good skips rollback |
| deploy.health_check_timeout_secs | 60 | post-deploy health check wait before failing |

## Edge cases and limits

- **Builder failure vs boot failure.** A failed build stops at step 1 and never reaches the health gate; only a successful build that then fails to bind its port reaches step 2. The B001 ticket is always step 1.
- **Port collisions self-heal narrowly.** When `up -d --build` fails with "port is already allocated", `DockerComposeDeploy::deploy()` retries up to two rounds and evicts only projects deemed evictable by `evictable_project()` (`cox-*` preview projects). It never evicts the live hub or shared infra — those are reported as collisions.
- **Down-before-up.** The adapter runs best-effort `docker compose -p <proj> down --remove-orphans` before deploying so a stale container can't hold ports.
- **Port 0 rejected.** A configured host_port of 0 is an unpublishable sentinel rejected by `is_publishable_host_port()`, folded into config load (COX-B042) rather than probed forever.
- **Timeout becomes PortError.** Overrunning DEPLOY_TIMEOUT returns ``PortError::Backend("docker compose timed out")``, routed through the same fail path as an unhealthy deploy (bug filed + possible rollback).
- **No compose file → skip.** A project without any recognized compose file reports success with deployed=false ("no compose file — deploy skipped"), so non-dockerised projects don't error the cycle.
- **Duplicate-bug suppression.** `file_deploy_bug` returns None (no new ticket) when an open Bug already starts with the Deploy-failing MARKER, so repeated failed deploys of the same stack don't pile up duplicate tickets.

## Code map

- crates/infrastructure/src/deploy/docker_compose.rs — DockerComposeDeploy adapter (deploy/down/ensure_daemon/health/health_check), compose_project_name, evictable_project, resolve_deploy_secrets; inline unit tests for secrets & health.
- crates/infrastructure/src/deploy/mod.rs — module wiring for the deploy adapters.
- crates/infrastructure/src/deploy/scoped_tests.rs — scoped test-command selection for run_tests_scoped.
- crates/application/src/ports/outbound/deploy.rs — DeployPort trait, verify_deploy_health(), parse_deploy_host_port(), is_publishable_host_port().
- crates/application/src/config.rs — struct DeployConfig and its defaults (host_port/enabled/auto_rollback/max_rollback_age_secs/migration_detection_paths/health_check_timeout_secs).
- crates/application/src/config_parse.rs — governing that rejects an out-of-range or zero deploy.host_port.
- crates/application/src/use_cases/cycle/mod.rs — RunCycleUseCase::execute wiring of deploy + health gate, and describe_health_failure().
- crates/application/src/use_cases/cycle/ops.rs — record_deploy(), file_deploy_bug() (the Deploy-failing MARKER), attempt_rollback(), record_known_good().
- deploy/docker-compose.cxa.yml — the CXA-B001 fix: a dedicated compose file for this repo's own backend under project cxa-backend, so it stops colliding with the web-app compose project.
- crates/app/tests/deploy_smoke.rs — integration smoke: brings a compose stack up and asserts it answers on the published port.

## Related

- wiki/engineering/docker-compose-deploys.md — companion page for this exact ticket class; also FOLDER Deployment.
- wiki/engineering/deployment.md — deploy-secret resolution that keeps an automated up from dying at interpolation (CXA-B017 and successors).
- docs/DEPLOYMENT.md — deployment shapes (macOS app, Docker Compose, Helm), env vars, split binaries.
