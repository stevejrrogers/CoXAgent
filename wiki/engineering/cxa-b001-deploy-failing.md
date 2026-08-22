FOLDER: -
# Docker Compose Deploy Failure (CXA-B001)

**Keywords:** docker compose, deploy failing, docker-desktop build details, host_port, deploy.host_port, compose project collision, cxa-backend, DockerComposeDeploy, DeployPort

## Overview

CXA-B001 was an incident report — "Deploy failing: docker compose failed: View build details: `docker-desktop://dashboard/build/default/default/nsm2qekx84qjmnuf5jaal0oeg`" — filed when an automated app-driven deploy ran `docker compose up -d --build` on a project and it returned non-zero. The "docker compose failed:" prefix is produced verbatim by the CoXAgent deploy adapter; the URL is Docker Desktop's deep link into the failing image-build log (compose project `default`, meaning no deterministic project name had been assigned yet). This page documents that failure surface and the two root causes PR #62 addressed: a malformed `deploy.host_port` that could poison config load and silently skip the health gate, and a docker-compose service/project-name collision between CoXAgent's own containers. It is for anyone reading a "docker compose failed" report or changing the self-host / agent-driven deployment path.

## How it works

Every agent-managed codebase carrying a compose file (`docker-compose.yml|yaml`, `compose.yml|yaml`) deploys through one adapter:

1. **Host port assignment** happens at onboarding via [`assign_host_port`](crates/app/src/host_port.rs), so one project never clashes with another on the same host; the chosen port persists as `deploy.host_port` in the project's `coxagent.json`.
2. On config load ([`load_config_with_probe`](crates/app/src/config_load.rs)) two distinct things happen: a `host_port` of the wrong type / out of range fails *parse* ([`config_parse::parse_config`], COX-B043) — config does not load, naming the field `deploy.host_port`. A value that does deserialize but is non-publishable (`0`), plus explicit-null and absent-key, is *healed* to a free port by [`heal_host_port()`](crates/app/src/config_load.rs).
3. The run cycle calls [`DockerComposeDeploy::deploy(work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs), which derives a deterministic project name ([`compose_project_name`], format `cox-<parent>-<dir>`), refuses if it collides with the live hub ([`evictable_project`]), resolves required `${VAR:?}` secrets once per pass ([`resolve_deploy_secrets_in()`](crates/infrastructure/src/deploy/docker_compose.rs)) then runs ``docker compose -p <proj> up -d --build``.
4. On non-zero exit it self-heals once for "port is already allocated" by evicting only evictable preview projects; otherwise it surfaces ``format!("docker compose failed: {detail}")`` (line ~1299) — exactly what CXA-B001 quoted.
5. A mandatory post-deploy probe runs before a deploy counts as successful. The run cycle polls `/` on the published port via [`run_health_check()`](crates/application/src/use_cases/cycle/ops.rs) → [`DeployPort::wait_healthy(port)`]; the yes/no rollback gate and every other call site go through the shared pure helper [`verify_deploy_health(deploy: &Arc<dyn DeployPort>, host_port: Option<u16>) -> bool`](crates/application/src/ports/outbound/deploy.rs).

PR #62 landed two changes:

- **Config hardening** (`config_load.rs`) — an out-of-range host port now fails config *load* naming that field instead of booting healthy on defaults with health checking silently disabled; explicit-null and absent-key ports heal to an assigned free port rather than erroring.
- **Compose collision fix** (`deploy/docker-compose.cxa.yml`) — CoXAgent's own root `docker-compose.yml`, its split deploy, and other stacks all named their Postgres service `db`, and compose keys volumes as `<project>_db`. Running under its own project name (`cxa-backend`) publishing 5433/6379 keeps CXA's backend separate from web/split deployments and mounts CXA's existing data volume directly instead of creating an empty one.

## Usage

Reproduce / verify against this repo itself using CI's exact gate:

```sh
# Bring up this repo's stack exactly as documented (both vars required):
PG_PASSWORD=ci-smoke COXAGENT_ADMIN_PASSWORD=ci-smoke \
  docker compose up -d --build
curl -fsS http://localhost:8101/
docker compose down -v

# Or run CI's deploy smoke test directly:
cargo test -p coxagent-app --test deploy_smoke -- --ignored --nocapture
```

Run CoXAgent's standalone native backend without clashing with its own web stack:

```sh
cd deploy
PG_USER=... PG_PASSWORD=... REDIS_PASSWORD=... \
  docker compose -f docker-compose.cxa.yml up -d
```

Interpreting failures you see:

- An app-driven message beginning "docker compose failed:" means [`DockerComposeDeploy::deploy`](crates/infrastructure/src/deploy/docker_compose.rs) saw non-zero exit from `up`.
- Local builds under Docker Desktop surface as "View build details:" plus ``docker-desktop://dashboard/build/<...>/<id>``; open that link to read which step broke.

## Interface

Trait boundary (the decision side stays pure over what adapters return):

- **Trait** [`DeployPort`](crates/application/src/ports/outbound/deploy.rs): methods include `lint`, `lint_report`, `cross_target_check`, `run_tests(_scoped)`, `down`, `health(port) -> bool`, `health_check(port)` (full status/time), `ensure_daemon()` (starts Docker Desktop on macOS via ``open -a Docker``), and `deploy(work_dir)` → `DeployReport { success, deployed, summary }`.
- **Adapter** [`DockerComposeDeploy::new()`](crates/infrastructure/src/deploy/docker_compose.rs).
- Host-port parsing / gating helpers:
  - `parse_deploy_host_port(raw_config) -> Result<Option<u16>, ()>`
  - `is_publishable_host_port(port) -> bool` (`port != 0`)
  - `verify_deploy_health(deploy: &Arc<dyn DeployPort>, host_port: Option<u16>) -> bool`
- Standalone backend layout keyed for CXA-B001:
  ```yaml
  # deploy/docker-compose.cxa.yml
  name: cxa-backend            # distinct project → avoids service/project-name collision
  services.db    → publish :5433 , mounts external volume coxagent_db
  services.redis → publish :6379 , command requirepass ${REDIS_PASSWORD}
  ```

## Configuration

Settings consulted by this path (defaults first where applicable):

| Setting | Default | Effect |
|---|---|---|
| Required `${VAR:?}` keys (`PG_PASSWORD`, … ) | must resolve | Compose aborts startup if unset; adapter seeds only missing ones |
| Operator-supplied secrets (process env or `.env`) | honoured verbatim | Never overridden by generated fallbacks |
| Generated fallback secret | random + persisted store | Only when nothing configured anywhere; stable per-project across cycles |
| Stored-file mode | owner-only (`0600`) on Unix | Hardens credentials at rest every resolution pass |

Env vars consulted by this path:

| Variable | Default | Effect |
|---|---|---|
| PG_PASSWORD / COXAGENT_ADMIN_PASSWORD | random per-pass fallback seeded into compose env; persisted out-of-tree at `deploy_secrets_root()` (default `$HOME/.local/share/coxagent/deploy-secrets/<hash>.env`, mode 0600) | Resolve `${VAR:?}` interpolation on secret-bearing compose files; honour an operator's value verbatim when provided |
| COXAGENT_DEPLOY_SECRETS_DIR | `$HOME/.local/share/coxagent/deploy-secrets` (else temp dir) | Overrides where per-project deploy-secret stores live |
| COXAGENT_DEPLOY_CPUS / COXAGENT_DEPLOY_MEM | `2` cpus / `1g` memory (`apply_resource_limits`) after a successful up via `docker update` | Clamp every container of a project; set either to `off` to skip |

Config-layer settings ([DeployConfig](crates/application/src/config.rs)):

| Setting | Default | Effect |
|---|---|---|
| deploy.enabled | true (default_true) | Whether the cycle deploys at all; turn off for stacks that would collide with live infra |
| deploy.host_port | None -> auto-assigned from PORT_BASE (8100+) at onboarding ([assign_host_port]); healed on load ([heal_host_port]) if explicit-null or absent; non-publishable values fail config load naming that field (COX-B043) | Host port published by the deployed app; must be publishable/non-zero ([is_publishable_host_port]) |
| deploy.auto_rollback / max_rollback_age_secs | false / 3600 s (`default_max_rollback_age_secs`) | Auto-redeploy the last known-good sha when a forward deploy or its post-deploy test fails; skip rollback if the good deploy is older than this |
| deploy.health_check_timeout_secs | 60 s (`default_health_check_timeout_secs`) | How long the mandatory post-deploy health check waits before the deploy is marked failed and auto-rollback triggers |
| deploy.migration_detection_paths | ["migrations"] (`default_migration_detection_paths`) | Repo-relative prefixes marking a DB migration; if any file under one changed since the known-good sha, rollback is skipped |

## Edge cases and limits

- **No compose file** -> deploy is skipped, not an error: DockerComposeDeploy::deploy returns success=true, deployed=false. A "docker compose failed" report therefore implies a compose file existed.
- **Port squatted by another project** -> self-heals up to two rounds by evicting only *evictable* preview projects (`cox-*`, but never `coxagent*` or `cox-infra*`). If nothing visible holds the port it gives up and reports rather than looping forever.
- **Health gate can misjudge deliberately**: any non-5xx answer from `/` counts as healthy — deployed projects are arbitrary and need no root route; a bare TCP connect that succeeds also passes. A genuinely dead-on-arrival container fails only because nothing binds the port at all.
- **What it deliberately does NOT do**: it does not build or fix images itself — it runs the build command, captures output, and files a bug for an agent to fix. The docker-desktop:// deep link is not generated by this code; it is Docker Desktop's own CLI hint pasted through verbatim in captured stderr/stdout.
- **Timeout bounds**: `up -d --build` is killed after 900s (`DEPLOY_TIMEOUT`, line 18); clippy after 600s per lint/lint_report/cross_target_check invocation with kill_group cleanup on timeout; each health probe bounded at 5s.

## Code map

- crates/infrastructure/src/deploy/docker_compose.rs — `DockerComposeDeploy` adapter implementing `DeployPort`: secret resolution (`resolve_deploy_secrets_in`, `missing_required_secrets`, out-of-tree store + legacy adoption/superseded retirement), project naming + eviction guard (`compose_project_name`, `evictable_project`), port-eviction retry loop, resource clamp (`apply_resource_limits`), compose-build cross-check fallback (`compose_build_check`, `build_failure_check`); produces the exact `` "docker compose failed: {detail}" `` summary (~line 1299).
- crates/infrastructure/src/deploy/mod.rs — re-exports the deploy submodules.
- crates/infrastructure/src/deploy/scoped_tests.rs — narrows a test run to what a change can reach.
- crates/infrastructure/src/lib.rs — crate-level re-exports used by the composition root.
- crates/application/src/ports/outbound/deploy.rs — trait `DeployPort` plus pure helpers `verify_deploy_health`, `parse_deploy_host_port`, `is_publishable_host_port`.
- crates/app/src/builders.rs and crates/app/src/lib.rs — composition root wiring `.with_deploy(Arc::new(<adapter>::new()))`, where `<adapter>` is the deploy adapter from docker_compose.rs named in the first Code map bullet.

- crates/app/src/host_port.rs — onboarding port assignment (`assign_host_port`) and `PORT_BASE`.
- crates/app/src/config_load.rs — load-time heal/reject of host_port (`heal_host_port`).
- crates/application/src/config.rs — `DeployConfig` struct (`enabled`, `host_port`, `auto_rollback`, `max_rollback_age_secs`, `health_check_timeout_secs`, `migration_detection_paths`).
- crates/application/src/use_cases/cycle/mod.rs — `RunCycleUseCase::execute` calls `deploy.deploy(&self.work_dir)` (line 963), runs the health gate + records/fails/bugs, drives `attempt_rollback` / `record_known_good`.
- crates/application/src/use_cases/cycle/ops.rs — `file_deploy_bug`, `record_deploy`, `run_health_check`, `attempt_rollback`.

## Related

- CXA-B010 (PG_PASSWORD missing) — sibling page in this folder on the `${VAR:?}` secret-injection failure mode.
- COX-F005 / COX-B004 / COX-B018 / COX-B053 — mandatory post-deploy health gate that turns "compose exited 0 but nothing listens" into a deploy failure; a 5xx at `/` or an unbound port both fail it.
- COX-B042 / CXA-B043 / CXA-B025 / CXA-B026 — deploy.host_port parse/reject/heal rules feeding the gate probe port.
