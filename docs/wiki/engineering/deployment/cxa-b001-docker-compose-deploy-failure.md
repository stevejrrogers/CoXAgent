FOLDER: Deployment
# Docker Compose Deploy Failure (CXA-B001)

**Keywords:** docker compose, deploy failing, docker-desktop build details, host_port, deploy.host_port, compose project collision, cxa-backend, DockerComposeDeploy, DeployPort

## Overview

CXA-B001 was an incident report — "Deploy failing: docker compose failed: View build details: `docker-desktop://dashboard/build/default/default/nsm2qekx84qjmnuf5jaal0oeg`" — filed when an automated app-driven deploy ran `docker compose up -d --build` on a project and it returned non-zero. The "docker compose failed:" prefix is produced verbatim by the CoXAgent deploy adapter; the URL is Docker Desktop's deep link into the failing image-build log (compose project `default`, meaning no deterministic project name had been assigned yet). This page documents that failure surface and the two root causes PR #62 addressed: a malformed `deploy.host_port` that could poison config load and silently skip the health gate, and a docker-compose service/project-name collision between CoXAgent's own containers. It is for anyone reading a "docker compose failed" report or changing the self-host / agent-driven deployment path.

## How it works

Every agent-managed codebase carrying a compose file (`docker-compose.yml|yaml`, `compose.yml|yaml`) deploys through one adapter:

1. **Host port assignment** happens at onboarding via [`assign_host_port`](crates/app/src/host_port.rs), so one project never clashes with another on the same host; the chosen port persists as `deploy.host_port` in the project's `coxagent.json`.
2. On config load ([`load_config_with_probe`](crates/app/src/config_load.rs)), [`heal_host_port`] rejects a non-publishable port and either fails the load naming that field (COX-B043) or heals explicit-null / absent to a free port.
3. The run cycle calls [`DockerComposeDeploy::deploy(work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs), which derives a deterministic project name ([`compose_project_name`], format `cox-<parent>-<dir>`), refuses if it collides with the live hub ([`evictable_project`]), resolves required `${VAR:?}` secrets once per pass ([`resolve_deploy_secrets_in`) then runs ``docker compose -p <proj> up -d --build``.
4. On non-zero exit it self-heals once for "port is already allocated" by evicting only evictable preview projects; otherwise it surfaces ``format!("docker compose failed: {detail}")`` (line 1152) — exactly what CXA-B001 quoted.
5. A mandatory post-deploy probe polls `/` on the published port via [`verify_deploy_health(port)`] before any gate passes.

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

- An app-driven message beginning "docker compose failed:" means [`DockerComposeDeploy::deploy] saw non-zero exit from `up`.
- Local builds under Docker Desktop surface as "View build details:" plus ``docker-desktop://dashboard/build/<...>/<id>``; open that link to read which step broke.

## Interface

Trait boundary (the decision side stays pure over what adapters return):

- **Trait** [`DeployPort](crates/application/src/ports/outbound/deploy.rs): methods include `lint`, `lint_report`, `cross_target_check`, `run_tests(_scoped)`, `down`, [`health(port) -> bool]`, [`health_check(port)` → full status/time], [`ensure_daemon()`] (starts Docker Desktop on macOS via ``open -a Docker``), and [`deploy(work_dir)` → [[`DeployReport { success, deployed, summary }]`.
- **Adapter** [`DockerComposeDeploy::new()`].
- Host-port parsing / gating helpers:
  - [``parse_deploy_host_port(raw_config) -> Result<Option<u16>, ()>``]
  - [``is_publishable_host_port(port) -> bool``] (`port != 0`)
  - [``verify_deploy_health(deploy, host_port) -> bool``]
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
