FOLDER: Deployment
# Docker Compose Deploy Failure (CXA-B001)

**Keywords:** docker compose failed, deploy failing, docker-desktop build details, deploy.host_port, host port collision, compose project name cox-, evictable_project, verify_deploy_health, DockerComposeDeploy, DeployPort

## Overview

CXA-B001 was an incident report — "Deploy failing: docker compose failed: View build details:
`docker-desktop://dashboard/build/default/default/nsm2qekx84qjmnuf5jaal0oeg`" — filed when an
automated app-driven deploy ran `docker compose up -d --build` on a project and it returned non-zero.
The `docker compose failed:` prefix is produced verbatim by the CoXAgent deploy adapter; the
`docker-desktop://…` URL is Docker Desktop's deep link into the failing image-build log (compose
project `default`, meaning no deterministic project name had been assigned yet). This page documents
that failure surface end to end — how an agent deploys a codebase through one adapter, the mandatory
health gate every call site runs afterwards, and what actually breaks when you read one of these
reports. It is for anyone triaging a "docker compose failed:" message or changing any part of the
self-host / agent-driven deployment path.

## How it works

Every agent-managed codebase carrying a compose file (`docker-compose.yml|yaml`, `compose.yml|yaml`)
deploys through one adapter ([`DockerComposeDeploy`](crates/infrastructure/src/deploy/docker_compose.rs)),
declared behind the [`DeployPort`](crates/application/src/ports/outbound/deploy.rs) trait boundary:

1. **Host-port assignment happens at onboarding**, not at deploy time.
   [`assign_host_port(base)`](crates/app/src/host_port.rs) scans ports already claimed by registered
   projects plus ports published by running containers (`ports_published_by_containers`) and writes the
   lowest free port from 8100 into `<project>/coxagent.json` under `deploy.host_port`.
2. **Config load validates/heals that value.** [`load_config_with_probe(state_dir)`](crates/app/src/config_load.rs)
   parses once; [`heal_host_port(root,&path,&mut cfg)`] leaves a publishable port alone (`!= 0`,
   per [`is_publishable_host_port`]), warns-once-and-replaces a zero/out-of-range value with a fresh free port,
   and derives `host_port_probe: Result<Option<u16>, ()>` so the raw document's bad value can never be folded
   into "nothing configured".
3. **The cycle calls** [`DockerComposeDeploy::deploy(work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs):
   it skips cleanly (`success=true`, `deployed=false`) when no compose file exists; otherwise it derives a
   deterministic project name via [`compose_project_name(work_dir)`] (`cox-<parent>-<dir>`,
   lowercase-alphanumerics/dashes sanitised), refuses anything that collides with the live hub via
   [`evictable_project(project)`] (anything named `coxagent*`, `cox-infra*`, or not starting with `cox-`
   is protected), does best-effort `down --remove-orphans` first so stale containers don't hold ports,
   resolves `${VAR:?}` secrets once per pass via [`resolve_deploy_secrets(work_dir)`], then runs
   `docker compose -p <proj> up -d --build` under DEPLOY_TIMEOUT (900s).
4. **On non-zero exit** it self-heals only for "port is already allocated": it extracts the bound port via
   [`extract_bind_port(err)`], identifies any squatter via [`compose_project_on_port(port)`] /
   [`container_on_port(port)`], evicts *only* evictable preview projects (else reports a collision), sleeps,
   retries once — then surfaces everything else as exactly what CXA-B001 quoted:

    ```rust
    format!("docker compose failed: {detail}") // last meaningful stderr/stdout line or exit code; docker_compose.rs:1299
    ```

5. **A mandatory post-deploy gate runs before any success is reported.** Every call site funnels through shared helpers in [deploy.rs](crates/application/src/ports/outbound/deploy.rs):
     - cycle path ([cycle/ops.rs]:[verify_health_after_deploy] / [run_health_check]) uses its stored probe;
     - chat's deploy command ([run_chat_reply.rs]:801) and PR preview ([forge.rs]:707) each call [parse_deploy_host_port(raw)] → [verify_deploy_health(deploy, Option<u16>)], which polls TCP health up to ATTEMPTS=15 × 2s; any error counts as unhealthy.

The trait keeps decision logic pure over what adapters return — application call sites never spawn docker,
they only interpret [`DeployReport{success, deployed, summary}`].

## Usage

Reproduce / verify against this repo itself using CI's exact gate:

```sh
# Bring up this repo's stack exactly as documented (both vars are REQUIRED):
PG_PASSWORD=ci-smoke COXAGENT_ADMIN_PASSWORD=ci-smoke \
  docker compose up -d --build          # root docker-compose.yml publishes :8101 -> container :4000

curl -fsS http://localhost:8101/
docker compose down -v                  # always tear down so nothing holds host port 8101 for another run

# Or run CI's own regression test for this surface (.github/workflows/ci.yml job 'deploy-smoke'):
cargo test -p coxagent-app --test deploy_smoke -- --ignored --nocapture    # crates/app/tests/deploy_smoke.rs
```

Run CoXAgent's standalone native backend without clashing with its own web stack:

```sh
cd deploy && PG_USER=... PG_PASSWORD=... REDIS_PASSWORD=... \
  docker compose -f docker-compose.cxa.yml up -d      # project cxa-backend; Postgres :5433 (+Redis :6379)
```

Interpreting failures you see:

- A message beginning **"docker compose failed:"** means [`DockerComposeDeploy::deploy`] saw non-zero exit;
  everything after the colon comes from stderr/stdout.
- A local build on macOS surfaces as "View build details:" plus `docker-desktop://dashboard/build/<project>/<id>` —
  open that deep link to read exactly which step broke inside Docker Desktop.

## Interface

Trait boundary ([crates/application/src/ports/outbound/deploy.rs](crates/application/src/ports/outbound/deploy.rs)):

```rust
pub trait DeployPort {
    async fn deploy(&self, work_dir: &Path) -> Result<DeployReport, PortError>;
    async fn ensure_daemon(&self) -> Result<bool>;   // starts Docker Desktop(macOS)/systemd(Linux), polls ~30x2s via daemon_up()
    async fn down(&self, _work_dir: &Path);          // teardown; default no-op (adapter uses "--remove-orphans")
    async fn health(&self, port: u16) -> Result<bool>;      // TCP liveness on 127.0.0.1:<port>
    async fn health_check(&self, port: u16) -> HealthCheckResult;
        // HTTP GET "http://127.0.0.1:<port>/"; passed/http_status/response_time_ms (COX-F005)
    async fn wait_healthy(&self, port: u16, timeout: Duration) -> HealthCheckResult;
        // polls health_check every 2s until it passes or the bound elapses
}

struct DeployReport { success: bool /* command ok */ , deployed: bool /* an actual stack ran */ , summary }
struct LintReport { errors: u64, sample: String, files: Vec<String> }
struct CrossCheck { available: bool /* false = check could not run */ , reason why-not (shown to humans), errors-vec of compiler lines for the foreign target }
```

Types / helpers beside the trait:

```rust
is_publishable_host_port(port: u16) -> bool     // == port != 0 ; only unpublishable u16 is 0 (COX-B042)
parse_deploy_host_port(raw_config: &str) -> Result<Option<u16>, ()>
     // Err(()) only when deploy.host_port is present but NOT a publishable u16;
     // missing key / explicit null / unreadable file => Ok(None)
verify_deploy_health(deploy: &Arc<dyn DeployPort>, host_port: Option<u16>) -> bool
     // None => passes without probing; else poll deploy.health(port), ATTEMPTS=15 @2s;
     // any probe error counts as unhealthy (never skips the gate)
```

Compose helpers inside [crates/infrastructure/src/deploy/docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs):

```rust
COMPOSE_FILES        ["docker-compose.yml","docker-compose.yaml","compose.yml","compose.yaml"]
REQUIRED_SECRET_KEYS ["PG_PASSWORD","COXAGENT_ADMIN_PASSWORD"]   // drives ${VAR:?} interpolation
DEPLOY_TIMEOUT       Duration::from_secs(900)

random_secret()                 crypto-random alphanumeric fallback secret (32 chars), look-alike-safe charset
missing_required_secrets(env_fn,&dot_env_keys)->Vec           pure over inputs => unit-testable precedence branches
resolve_deploy_secrets(work_dir)->Vec<(String,String)>        env > .env > random fallback precedence,
                                                              computed ONCE per pass so an eviction retry re-runs identical interpolation
seed_deploy_secrets(cmd, &[(String,String)])                  sets VAR on the child env for ${VAR:?}
read_dot_env(path), read_stored_secrets(path), write_stored_secrets_owned(...),
   store_file(root,&dir)                      keyed by SHA256 of compose_project_name, NOT disk path
compose_project_name(dir)->String                            cox-<parent>-<dir>, lower+alnum->'-', truncate60
evictable_project(name)->bool                                true only for cox-* previews ≠ live hub / cox-infra
extract_bind_port(err), compose_project_on_port(pubPort), container_on_port(pubPort)
apply_resource_limits(proj)                    docker update --cpus --memory to every container after up
daemon_up(), ensure_daemon()                    Docker Desktop on macOS / systemctl start docker on Linux
```

## Configuration

**Required-secret precedence** (drives `${VAR:?}` interpolation so compose starts without a known credential):

1. **Process environment** — if this process already carries a non-blank value for a key in `REQUIRED_SECRET_KEYS`
   (`PG_PASSWORD`, `COXAGENT_ADMIN_PASSWORD`), it is left alone.
2. **Project-dir `.env`** — an operator-authored assignment is parsed by `read_dot_env` and honoured verbatim.
3. **Fallback** — only keys missing from both get a fresh cryptographically-random value from `random_secret()`
   (32 chars, look-alike-safe charset). This is what makes B010's "required variable X is missing" not recur.

**Secret durability**: fallback secrets are persisted out-of-tree, keyed by SHA-256 of the compose *project name*
(`store_file`), not the disk path, and reused across cycles so pgdata volumes keep their superuser password.
Store files are hardened to owner-only `0600` on every pass (`repair_store_permissions`). The store root defaults
to `~/.local/share/coxagent/deploy-secrets`, overridable with `COXAGENT_DEPLOY_SECRETS_DIR`.

**Deploy limits:**

| Setting | Default | Behaviour |
|---|---|---|
| `COXAGENT_DEPLOY_CPUS` | `2` | CPU budget per container via `apply_resource_limits` (`docker update --cpus`) |
| `COXAGENT_DEPLOY_MEM` | `1g` | Memory budget per container (`--memory --memory-swap`) |
| (set either to `off`) | — | skips clamping entirely |
| `DEPLOY_TIMEOUT` (const) | 900s | bounds each single compose invocation |
| health gate (const) | 15 × 2s | polls TCP liveness after deploy |

The canonical backing-service env list lives in [DEPLOYMENT.md](DEPLOYMENT.md) at repo root — e.g.
`COXAGENT_DB_DSN`, `COXAGENT_REDIS_URL`, S3/Mongo vars, admin user/password, port 4000 default.

## Edge cases and limits

- **No compose file**: deploys skip cleanly (`success=true`, `deployed=false`) rather than erroring —
  non-dockerised projects do not fail the cycle.
- **Port already allocated**: self-healed once only when the squatter is an evictable preview project or a raw
  container; otherwise it surfaces as a failure with detail text. The live hub (`coxagent*`) and shared infra
  (`cox-infra*`) are never evicted — attempting that yields an explicit refusal error instead of an outage.
- **Malformed host_port** (zero, negative, out-of-range u16): treated as corrupt config. At load it fails/heals loudly
  rather than booting healthy with health checking disabled; at deploy time [`parse_deploy_host_port`] returns
  [`Err(())] so [`verify_deploy_health] reports unhealthy rather than passing unchecked.
- **Probe errors never pass**: an unreachable endpoint or an erroring probe counts as unhealthy — there is no way to
  skip the gate by breaking the check itself.
- **Secrets**: compose aborts if a required `${VAR}` has no value anywhere; behaviour intentionally refuses public,
  source-published constants for real deployments.
- Not covered here: build-stage compiler failures inside an image show up only as Docker Desktop build details plus one
  retry-free failure report here.

## Code map

- crates/infrastructure/src/deploy/docker_compose.rs — DockerComposeDeploy adapter implementing DeployPort;
  COMPOSE_FILES detection, compose_project_name/evictable_project guards, deploy() up → evict-retry → summarize flow that emits "docker compose failed:", ensure_daemon()/daemon_up(), secret resolution+seed functions, apply_resource_limits(), extract_bind_port/compose_project_on_port/container_on_port()
- crates/application/src/ports/outbound/deploy.rs — DeployPort trait + DeployReport/LintReport/CrossCheck types + parse_deploy_host_port / is_publishable_host_port / verify_deploy_health helpers + wait_healthy default impl
- crates/app/src/host_port.rs — assign_host_port() at onboarding (+ ports_published_by_containers)
- crates/app/src/config_load.rs — load_config_with_probe() + heal_host_port() validating/healing deploy.host_port at load; probe_from_raw()
- crates/app/src/builders.rs & crates/app/src/lib.rs:1066 — wiring Arc::new(DockerComposeDeploy::new()) into builders
- crates/app/tests/deploy_smoke.rs + .github/workflows/ci.yml job 'deploy-smoke'/'deploy-build' — regression gates reproducing this failure surface
- Deploy call sites behind the shared gate:
    - crates/application/src/use_cases/cycle/ops.rs — verify_health_after_deploy() / run_health_check()
    - crates/presentation/src/server/pr_preview_tests.rs — PR-preview path guard tests
    - crates/presentation/src/server/forge.rs — PR preview: parse_deploy_host_port → verify_deploy_health
    - crates/presentation/src/server/store_rpc.rs & chat.rs — store/reply surfaces
- docker-compose.yml            root web app stack (host :8101 → container :4000)
- deploy/docker-compose.cxa.yml standalone native backend, project cxa-backend (:5433/:6379)
- deploy/docker-compose.split.yml production split shape with Caddy → realtime routing

## Related

This page documents the deployment pipeline CXA-B001 surfaced. Sibling incident pages share these same compose files and adapter:

- [CXA-B010 Postgres password missing](cxa-b010-pg-password-missing.md) — same files/adapter, different symptom ("required variable PG_PASSWORD..."), resolved together with B001's secret-seeding logic.
- [DEPLOYMENT.md](../..//DEPLOYMENT.md) — repo-root deployment guide: compose/K8s shapes, canonical env list, service binaries.
