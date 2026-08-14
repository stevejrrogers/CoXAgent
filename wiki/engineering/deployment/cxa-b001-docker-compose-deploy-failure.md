FOLDER: Deployment
# Docker Compose Deploy Failure (CXA-B001)

**Keywords:** docker compose, deploy failing, docker-desktop build details, deploy.host_port, host_port collision, compose project name, cox- prefix, DockerComposeDeploy, DeployPort, verify_deploy_health

## Overview

CXA-B001 was an incident report — "Deploy failing: docker compose failed: View build details: `docker-desktop://dashboard/build/default/default/nsm2qekx84qjmnuf5jaal0oeg`" — filed when an automated app-driven deploy ran `docker compose up -d --build` on a project and it returned non-zero. The `docker compose failed:` prefix is produced verbatim by the CoXAgent deploy adapter; the URL is Docker Desktop's deep link into the failing image-build log (compose project `default`, meaning no deterministic project name had been assigned yet). This page documents that failure surface and its root causes — a malformed or colliding `deploy.host_port`, and docker-compose service/project-name collisions between CoXAgent's own containers. It is for anyone reading a "docker compose failed" report or changing the self-host / agent-driven deployment path.

## How it works

Every agent-managed codebase carrying a compose file (`docker-compose.yml|yaml`, `compose.yml|yaml`) deploys through one adapter:

1. **Host port assignment** happens at onboarding via [`assign_host_port`](crates/app/src/host_port.rs), so one project never clashes with another on the same host; the chosen port persists as `deploy.host_port` in the project's `coxagent.json`.
2. On config load ([`load_config_with_probe`](crates/app/src/config_load.rs)), [`heal_host_port`] validates that value: a publishable port (non-zero) is left untouched; an absent key or explicit-null heals to a freshly assigned free port; an out-of-range or zero value warns once at load and is healed rather than silently disabling health checking.
3. The cycle calls [`DockerComposeDeploy::deploy(work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs). It skips (success = true but deployed = false) when no compose file exists; otherwise it derives a deterministic project name via [`compose_project_name`] (`cox-<parent>-<dir>`, sanitised lowercase alphanumerics/dashes, truncated to 60 chars), refuses to proceed if that collides with the live hub via [`evictable_project`] (anything named `coxagent*` or `cox-infra*` is protected), brings any prior stack down best-effort with ``down --remove-orphans``, resolves required `${VAR:?}` secrets once per pass via [`resolve_deploy_secrets`], then runs ``docker compose -p <proj> up -d --build`` under a 900s timeout.
4. On non-zero exit it self-heals only for "port is already allocated": it extracts the bound port ([`extract_bind_port`]) and evicts *only* evictable preview projects before retrying once; anything else surfaces ``format!("docker compose failed: {detail}")`` — exactly what CXA-B001 quoted.
5. A mandatory post-deploy gate ([`verify_deploy_health(deploy, host_port)`](crates/application/src/ports/outbound/deploy.rs)) polls `/127.0.0.1:<port>` up to 15 times at 2s intervals before any call site reports success.

The trait boundary keeps decision logic pure over what adapters return: application-side call sites (`cycle/ops.rs::verify_health_after_deploy`, `run_chat_reply.rs`, PR-preview in presentation/server/forge.rs) all funnel through [`parse_deploy_host_port(raw_config)`] → [`verify_deploy_health(...)`], so a malformed config fails the gate identically everywhere instead of each site re-deriving its own parse.

## Usage

Reproduce / verify against this repo itself using CI's exact gate:

```sh
# Bring up this repo's stack exactly as documented (both vars required):
PG_PASSWORD=ci-smoke COXAGENT_ADMIN_PASSWORD=ci-smoke \
  docker compose up -d --build
curl -fsS http://localhost:8101/
docker compose down -v
```

Run CoXAgent's standalone native backend without clashing with its own web stack:

```sh
cd deploy
PG_USER=... PG_PASSWORD=... REDIS_PASSWORD=... \
  docker compose -f docker-compose.cxa.yml up -d
```

Interpreting failures you see:

- An app-driven message beginning "docker compose failed:" means [`DockerComposeDeploy::deploy`] saw non-zero exit from `up`. The detail after the colon comes from stderr/stdout.
- Local builds under Docker Desktop surface as "View build details:" plus ``docker-desktop://dashboard/build/<project>/<id>``; open that link to read which step broke.

## Interface

Trait boundary ([crates/application/src/ports/outbound/deploy.rs](crates/application/src/ports/outbound/deploy.rs)):

- **Trait** [`DeployPort`]: async methods include:
  - `deploy(work_dir) -> Result<DeployReport, PortError>` — main entry.
  - `ensure_daemon() -> Result<bool>` — starts Docker Desktop (macOS) / systemd (Linux), polls ~30×2s for readiness.
  - `down(work_dir)` — teardown (`docker compose down --remove-orphans`, default no-op).
  - `health(port) -> Result<bool>` — TCP liveness check.
  - `health_check(port) -> HealthCheckResult{passed, http_status, response_time_ms}` (COX-F005).
  - `wait_healthy(port, timeout)` — polls every 2s until healthy or bound elapses.
  - lint / lint_report / run_tests(_scoped); default implementations skip gracefully.
- **Report** [`DeployReport{success: bool, deployed: bool, summary: String}`].
- **Adapter** [`DockerComposeDeploy::new()`](crates/infrastructure/src/deploy/docker_compose.rs).
- Host-port parsing / gating helpers:
    ```rust
    parse_deploy_host_port(raw_config) -> Result<Option<u16>, ()>
         // Err only when host_port present but NOT a publishable u16;
         // missing key / null / unreadable file => Ok(None)
    is_publishable_host_port(port) -> bool   // port != 0
    verify_deploy_health(&Arc<dyn DeployPort>, Option<u16>) -> bool
         // None => passes without probing; else poll health() ATTEMPTS=15 @2s;
         // any probe error counts as unhealthy (never skips the gate)
        ```
- Compose helper functions inside [crates/infrastructure/src/deploy/docker_compose.rs]:
    ```rust
    COMPOSE_FILES               // ["docker-compose.yml","docker-compose.yaml","compose.yml","compose.yaml"]
    REQUIRED_SECRET_KEYS        // ["PG_PASSWORD","COXAGENT_ADMIN_PASSWORD"]
    DEPLOY_TIMEOUT              // Duration::from_secs(900)
    random_secret()             // crypto-random alphanumeric fallback secret (32 chars)
    missing_required_secrets(&provided_by_env Fn(&str)->bool,
                             &dot_env_keys HashSet<String>) -> Vec<&'static str>
    resolve_deploy_secrets_in/resolve_deploy_secrets   // env > .env > random fallback precedence,
                                                       // computed ONCE per pass so an eviction retry re-runs identical interpolation
    seed_deploy_secrets(&mut Command,... )             // sets VAR/VAR values on child env for ${VAR:?}
    read_dot_env(path), write_stored_secrets_owned()   // owner-stamped store files (# owner=<hash of proj name>)
      ```

Standalone backend layout keyed for CXA-B001:
```yaml
# deploy/docker-compose.cxa.yml   → distinct project avoids service/project-name collision:
name: cxa-backend                 # separate from web/split deployments' implicit names,
                                  # so volumes are not confused with their <project>_db ones;
services.db     → publish :5433 , mounts external volume coxagent_db directly (reuses existing data,
                                  # does NOT create an empty new one)
services.redis  → publish :6379 , command redis-server ... requirepass ${REDIS_PASSWORD}
```
Other in-tree stacks sharing these patterns:
```yaml
# docker-compose.yml                  root web app → fixed host port published at :8101 (:4000 inside container),
                                      PG + admin password both REQUIRED (${VAR}:?)
# deploy/docker-compose.split.yml     production split shape — gateway/realtime/knowledge (+ runner profile off by default),
                                      Caddy routing sockets→realtime for scale-out per plane
```

## Configuration

The settings that change this path behave as follows:

**Required-secret precedence** (drives `${VAR:?}` interpolation so services start without a known credential):

1. Process environment — if this process already carries a non-blank value for a key in `REQUIRED_SECRET_KEYS` (`PG_PASSWORD`, `COXAGENT_ADMIN_PASSWORD`), it is left alone.
2. Project-dir `.env` — an operator-authored assignment is parsed by `read_dot_env` and honoured verbatim.
3. Fallback — only keys missing from both get a fresh cryptographically-random value from `random_secret()` (32 chars, look-alike-safe charset). This is what makes B010's failure surface ("required variable X is missing a value") not recur.

**Secret durability** (CXA-B031/B032/B036/B039): fallback secrets are persisted out-of-tree keyed by a hash of the compose *project name* (not disk path) and reused across cycles so pgdata volumes keep their superuser password; legacy path-keyed stores are adopted during upgrade. Store files are remediated to owner-only `0600` on every pass (CXA-B038).

**Deploy limits:**

- `COXAGENT_DEPLOY_CPUS` (default `2`) / `COXAGENT_DEPLOY_MEM` (default `1g`) — applied via `apply_resource_limits()` (`docker update --cpus --memory --memory-swap`) to every container of a successfully-started project; set either var to `off` to skip clamping.
- Other constants: `DEPLOY_TIMEOUT` = 900s bounds each `up` invocation; post-deploy health gate polls 15 × 2s.

Full canonical backing-service env list lives in [DEPLOYMENT.md](DEPLOYMENT.md) at repo root — e.g. `COXAGENT_DB_DSN`, `COXAGENT_REDIS_URL`, S3/Mongo vars, admin user/password, port 4000 default.

## Edge cases and limits

- **No compose file**: deploys skip cleanly (`success=true`, `deployed=false`) rather than erroring; non-dockerised projects do not fail the cycle.
- **Port already allocated**: self-healed once only when the squatter is an evictable preview project or raw container; otherwise it surfaces as a failure with detail text. The live hub and shared infra (`cox-infra`) are never evicted — attempting that yields an explicit refusal error instead of an outage.
- **Malformed host_port** (zero, negative, out-of-range u16): treated as corrupt config. At load it fails/heals loudly rather than booting healthy with health checking disabled; at deploy time `parse_deploy_host_port` returns `Err` so `verify_deploy_health` reports unhealthy rather than passing unchecked.
- **Probe errors never pass**: an unreachable endpoint or an erroring probe counts as unhealthy — there is no way to skip the gate by breaking the check itself.
- **Secrets**: compose aborts if a required `${VAR}` has no value anywhere; behaviour intentionally refuses public source-published constants for real deployments.
- Not covered here: build-stage compiler failures inside an image show up only as Docker Desktop build details plus one retry-free failure report here.

## Code map

- crates/infrastructure/src/deploy/docker_compose.rs — DockerComposeDeploy adapter implementing DeployPort; COMPOSE_FILES detection, compose_project_name/evictable_project guards, deploy() up/evict-retry/summary flow producing "docker compose failed:", ensure_daemon(), daemon_up(), secret resolution+seed functions, apply_resource_limits(), compose_build_check()
- crates/application/src/ports/outbound/deploy.rs — DeployPort trait + DeployReport/LintReport/CrossCheck types + parse_deploy_host_port / is_publishable_host_port / verify_deploy_health helpers
- crates/app/src/host_port.rs — assign_host_port() at onboarding
- crates/app/src/config_load.rs — load_config_with_probe() + heal_host_port() validating/healing deploy.host_port at load
- crates/app/src/builders.rs / lib.rs — wiring Arc::new(DockerComposeDeploy::new()) into builders
- crates/presentation/src/server — post-deploy gate call sites:
   - crates/presentation/src/server/pr_preview_tests.rs & forge.rs — PR-preview path: parse_deploy_host_port → verify_deploy_health
   - crates/presentation/src/server/store_rpc.rs & chat.rs — related store/reply surfaces
   - cycle gate lives in application: crates/application/src/use_cases/cycle/ops.rs::verify_health_after_deploy / run_health_check
- docker-compose.yml            root web app stack (:8101)
- deploy/docker-compose.cxa.yml standalone native backend project cxa-backend (:5433/:6379)
- deploy/docker-compose.split.yml production split shape w/Caddy→realtime routing
- scripts/docker-clean.sh       janitor sweep companion

## Related

Existing sibling incident pages share these same compose files:
[CXA-B010 Postgres password missing](docs/wiki/engineering/deployment/cxa-b010-pg-password-missing.md) —
same files/adapter, different symptom ("required variable PG_PASSWORD…"), resolved together with B001's seeding logic.
</content>
