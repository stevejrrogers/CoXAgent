# CoXAgent — Deployment

Three supported shapes, one codebase.

## 1. macOS app (developer / single team)

Build + run the desktop app; the hub starts embedded on port 4000.

```sh
./scripts/build-macos-app.sh
open desktop/build/CoXAgent.app
```

State lives under `~/CoXAgent/`. Optional shared backend: set
`COXAGENT_DB_DSN` / `COXAGENT_REDIS_URL` (env or `coordination.json` — the file
supports `${VAR}` placeholders and is auto-clamped to mode 600).

## 2. Docker Compose (self-host server)

Everything on one Linux host with a domain: app + Postgres + Redis + MinIO +
Mongo + coturn + Caddy (automatic HTTPS).

```sh
cd deploy
cp .env.example .env                    # fill in domain, admin password, secrets
../scripts/rotate-admin-password.sh     # generates ADMIN_PASSWORD for you
docker compose up -d
```

`.env` is gitignored and must stay that way — it holds the live super-admin
password and every backing-service credential. Only `.env.example`, with its
`change-me-…` placeholders, belongs in git (COX-B030: a working admin password
was once committed here, so it is public in this repo's history; any deployment
that ever used it must rotate). `scripts/rotate-admin-password.sh --restart`
rotates and recreates the app in one step; `cargo test -p coxagent-app --test
committed_secrets_gate` fails the build if a credential is ever committed again.

**Full split on one host** — the same 4-service shape as Helm (gateway /
realtime / knowledge as separate containers, Caddy routing sockets to
realtime), for independent scaling/restarts per plane:

```sh
cd deploy
docker compose -f docker-compose.split.yml up -d
docker compose -f docker-compose.split.yml up -d --scale gateway=3   # scale the API
```

The runner service is behind a `runner` profile and OFF by default — agent
CLIs (claude/opencode) and their logins live on operator machines, so run
`cox-runner` natively there; only enable the container if your image bundles
a CLI.

- TLS terminates at Caddy; the app adds `Secure` to session cookies
  automatically behind it.
- Backups: hub documents snapshot nightly to the app volume under
  `backups/YYYY-MM-DD/` (14-day retention). Restore = stop app, copy a
  snapshot's JSON over the corresponding app_kv docs (or re-import via psql),
  start app.
- Keep docker tidy on hosts where agents deploy previews:
  `./scripts/docker-clean.sh` (add `--deep` for builder cache).

## 3. Kubernetes (Helm)

```sh
kubectl create secret generic coxagent-secrets \
  --from-literal=db-dsn='postgres://…' \
  --from-literal=auth-dsn='postgres://…' \
  --from-literal=redis-url='redis://…' \
  --from-literal=admin-user='root' \
  --from-literal=admin-password='…' \
  --from-literal=s3-access-key='…' --from-literal=s3-secret-key='…'

helm install cox deploy/helm/coxagent \
  --set ingress.enabled=true \
  --set ingress.host=coxagent.example.com \
  --set ingress.tlsSecret=coxagent-tls
```

Chart layout mirrors the target architecture:

- **hub** Deployment (control plane) — stateless; `hub.replicas > 1` is valid
  only with the Redis event bus (the chart enforces this), non-root, health
  probes on `/api/health`, PVC for hub data.
- **runner** Deployment (execution plane) — the only pods that run agent
  engines/git/builds; resource-capped and separately imaged (its image must
  carry `claude`/`opencode`, git, docker CLI). Disable with
  `runner.enabled=false` if operators run on developer machines instead.
- Backing services are **external by design** (managed Postgres/Redis/S3);
  credentials come from an existing Secret, never from values.

## Environment variables (canonical list)

| Var | Purpose |
|---|---|
| `COXAGENT_DB_DSN` | Postgres — project state, claims, app_kv |
| `COXAGENT_AUTH_DSN` | Postgres — accounts/sessions (defaults to DB_DSN) |
| `COXAGENT_REDIS_URL` | Sessions, coordination, `cox:events` bus |
| `COXAGENT_S3_ENDPOINT/BUCKET/ACCESS_KEY/SECRET_KEY` | File storage |
| `COXAGENT_MONGO_URL/DB` | Docs store (optional) |
| `COXAGENT_ADMIN_USER/PASSWORD` | First-run Super Admin bootstrap |
| `COXAGENT_OPERATOR` | Headless runner identity (`name@host` attribution) |

## MCP (connect agents & IDE clients)

The hub speaks MCP at `POST /api/mcp` (streamable HTTP JSON-RPC) with the same
authz as the REST API. Tools: `search_symbols`, `symbol_refs`, `get_ticket`,
`pr_queue`, `report_blocker` — every call is audited.

`.mcp.json` for a project checkout (claude CLI / Claude Desktop / Cursor):

```json
{
  "mcpServers": {
    "coxagent": {
      "type": "http",
      "url": "http://127.0.0.1:4000/api/mcp",
      "headers": { "Authorization": "Bearer <coxagent API token>" }
    }
  }
}
```

**Self-service setup lives in Settings → MCP** (visible to every signed-in
user): mint a personal token there — it acts as *you* at your own role, never
an elevation — and copy the ready-made config for Claude Code (`.mcp.json`),
opencode (`opencode.json`), or any other MCP client. Personal tokens are
namespaced `user:<name>:<label>`, are listable/revocable only by their owner
(admins see all in Users → API tokens), and every mint/revoke is audited.
Admin-minted service-account tokens remain in Users → API tokens;
scope-check happens server-side per project argument.

## Service binaries (the physical split)

Five role binaries build from one codebase (`cargo build --workspace --bins`),
each its own crate under `crates/services/` (plus `cox-all` in the app crate);
each enforces its surface in-process (wrong endpoint → 503), so a mis-routed
load balancer fails loudly instead of leaking surfaces:

| Binary | Serves | Notes |
|---|---|---|
| `cox-all` | everything | self-host default — identical to `coxagent hub` |
| `cox-gateway` | REST + MCP + SPA | shell-free (`COXAGENT_NO_INLINE_EXEC` auto-set) |
| `cox-realtime` | WS/SSE (chat, docs, events, terminal) + health | scale on connections; needs the Redis bus |
| `cox-knowledge` | health + batch loops (budgets, backups, releases) | single replica |
| `cox-runner` | agent cycles, git, docker, queued jobs | env: COXAGENT_STATE_DIR + COXAGENT_WORK_DIR (+OPERATOR) |

Config is env-only for the role binaries: `COXAGENT_REGISTRY` (default
`~/CoXAgent/registry.json`), `COXAGENT_PORT` (default 4000), plus the backing
env vars above. Split deployments require Redis (`cox:events`) and an LB that
sends `/…/ws`, `/…/events`, `/…/terminal` to realtime pods — everything else to
gateway pods. Start with `cox-all`; split only when load asks for it.

## Upgrade & rollback

- The hub is safe to restart at any time: sessions persist, runners re-attach,
  an interrupted agent run is retried by the normal cycle machinery.
- Rolling upgrade order: runners → hub (contracts are versioned; frames with a
  different `CONTRACT_VERSION` are skipped, not misparsed).

## App distribution & in-app updates

1. Tag a release: `git tag v0.94.0 && git push origin v0.94.0` — the `desktop`
   workflow builds the full app bundles (macOS `.dmg`, Windows zip, Linux
   tar.gz) and attaches them to the GitHub Release. **Bump the workspace
   `version` in Cargo.toml in the same commit** (the update check compares it)
   and refresh `Cargo.lock` (CI builds `--locked`).
2. Point the hub at the repo once: Settings → Workspace → App downloads →
   `releases_repo`. The hub polls the latest release (2-minute cadence until
   the first release is seen, then every 30 minutes) and republishes version +
   per-platform URLs at `GET /api/app/latest`. iOS is always a manual App
   Store / TestFlight link.
3. **Private repos work**: clients never hit GitHub directly — the hub streams
   assets through `GET /api/app/download/{macos.dmg,windows.exe,linux.tar.gz}`
   using its own `gh` auth. Tokens never reach clients.
4. Update UX: when a newer version exists, every signed-in client shows a
   bottom-right toast (dismiss remembers the version) and the get-app button
   beside the user badge turns into a pulsing rocket. Inside the macOS shell,
   clicking macOS **self-updates in place**: download → mount → swap the .app
   → relaunch, with toast progress and a browser-download fallback on any
   failure. Other platforms download via the browser. The sidebar brand shows
   the running version (`autonomous dev team · vX.Y.Z`).

## Local backing services & docker hygiene

- `deploy/local-infra/docker-compose.yml` (project `cox-infra`) groups the
  local Postgres/Redis/Mongo/MinIO with their existing named volumes.
- Agent deploys are forced onto deterministic compose project names
  (`cox-<parent>-<dir>`); an hourly hub janitor `down`s fully-stopped `cox-*`
  projects (never `cox-infra`) and prunes dangling images. Manual sweep:
  `./scripts/docker-clean.sh` (`--deep` adds builder cache).
- Hub shim directories (`$TMPDIR/coxagent-shims-*`) are reclaimed at every hub
  start (CXA-B117): pid-suffixed dirs whose hub process is no longer running
  are removed, while the legacy shared `coxagent-shims/` directory (the
  pre-CXA-B109 format, possibly still advertised by an old hub) is rewritten
  with fallback-guarded scripts — never removed, so the old hub's agents keep
  working. No manual sweep needed; a hub restart does it.

## Standalone CXA backend (`deploy/docker-compose.cxa.yml`)

For running **only** Postgres + Redis as a shared backend for the *native CXA
hub* on this host (not the full web stack). It uses its own project name
(`cxa-backend`) and mounts CXA's existing data volume directly, so it never
collides with the root compose or `local-infra`.

```sh
cd deploy
cp .env.example .env     # fill in real secrets — do not reuse placeholders
docker compose -f docker-compose.cxa.yml up -d
```

Required variables (`${VAR:?}` — compose fails fast if any is unset):

| Var | Used by |
|---|---|
| `PG_USER` | `db` service — `POSTGRES_USER` + healthcheck |
| `PG_PASSWORD` | `db` service — `POSTGRES_PASSWORD` |
| `REDIS_PASSWORD` | `redis` service — redis auth (`--requirepass`) |

Set them inline instead of `.env`, e.g.:

```sh
PG_USER=coxagent PG_PASSWORD=<secret> REDIS_PASSWORD=<secret> \
  docker compose -f docker-compose.cxa.yml up -d
```

Host ports: Postgres on **127.0.0.1:5433**, Redis on **127.0.0.1:6379**
(loopback-only; matching what the native hub expects in coordination.json).
An unauthenticated Redis is an authentication bypass (live session keys), so
never run with an empty password.

## Migrate to another machine

Everything portable in one tarball + one command on the new machine:

```sh
# old machine
scripts/migrate-export.sh            # → cox-export-YYYYmmdd-HHMM.tar.gz
# new machine (Docker installed, repo cloned)
scripts/migrate-import.sh cox-export-YYYYmmdd-HHMM.tar.gz
```

Export carries Postgres (all state/users/tickets/KV), Mongo (wiki), MinIO
uploads, and `~/CoXAgent` configs. Import creates the volumes, brings up
`cox-infra`, and restores everything. Redis is ephemeral and codebases
re-clone from git. Credentials never travel: run `claude` + `gh auth login`
on the new machine, then launch the app — the team resumes from Postgres
exactly where it stopped.
