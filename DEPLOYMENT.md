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
  automatically behind it. **`DOMAIN` must be set** in `.env`: Caddy then
  provisions a certificate automatically and serves HTTPS on 443. With `DOMAIN`
  unset the front door falls back to plain HTTP on `:80` — local development
  only; never expose the hub on a network without `DOMAIN`, or session bearer
  tokens travel in clear text.
- Backups: hub documents snapshot nightly to the app volume under
  `backups/YYYY-MM-DD/` (14-day retention). Restore = stop app, copy a
  snapshot's JSON over the corresponding app_kv docs (or re-import via psql),
  start app.
- Keep docker tidy on hosts where agents deploy previews:
  `./scripts/docker-clean.sh` (add `--deep` for builder cache).

### Minimal self-host stack (repo root `docker-compose.yml`)

The repo root carries a minimal two-service compose — app published on host
port 8101 (override with `APP_PORT`) plus Postgres — for a self-host without
MinIO/Mongo/coturn:

```sh
PG_PASSWORD=<strong> COXAGENT_ADMIN_PASSWORD=<strong> docker compose up -d --build
# → http://localhost:8101, log in as root
```

**Password rotation & the pgdata volume (CXA-B115):** `POSTGRES_PASSWORD` is
applied only when the `pgdata` volume is **first initialized**. Changing
`PG_PASSWORD` against an existing volume does NOT update the password stored
inside it — the db healthcheck authenticates with `psql` on every probe, so a
mismatch makes `db` go **unhealthy** with `password authentication failed`
and `coxagent` never boots (fail-fast by design; the old `pg_isready` check
never authenticated, so the stack deployed "healthy" while the app silently
degraded). Recover without losing data:

```sh
docker compose exec db psql -U postgres -c "ALTER USER postgres PASSWORD '<new>'"
```

`exec` connects over the container's unix socket, which the image's pg_hba
keeps trust-authenticated — it works precisely because the stored password is
unknown. Deleting the `pgdata` volume also restores access but **wipes all
stored state**; prefer the `ALTER USER` path. The same first-init rule applies
to every Postgres volume in this repo (`deploy/docker-compose.yml`'s `db`,
`docker-compose.split.yml`'s `db`, `docker-compose.cxa.yml`'s
`source_cxa_data`).

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
| `COXAGENT_OIDC_*` | Optional SSO — see below (all unset ⇒ SSO off, password login only) |
| `COXAGENT_LLM_BASE_URL` / `COXAGENT_LLM_API_KEY` | Endpoint + key for the `http` engine — see *Agent engines* |

## Agent engines

Each role runs on an **engine**, chosen in `coxagent.json`'s `engine` block (a
`default` plus `per_role` overrides) or from the dashboard — expensive models
for DEV/SA, cheap ones for BA/DOCS. Most engines wrap a vendor CLI the runner
node must carry (`claude`, `opencode`, `gemini`, `codex`, `copilot`); `harxes`
is in-process.

**`http` — any OpenAI-compatible gateway (no CLI).** Set a role's engine to
`"http"` and its `model` to the gateway's model id; point the hub at the
endpoint with `COXAGENT_LLM_BASE_URL` (default `https://api.openai.com/v1`) and
`COXAGENT_LLM_API_KEY`. It calls `POST {base}/chat/completions` (key via
`Authorization: Bearer`), so it works with OpenAI, Azure OpenAI, vLLM, LiteLLM,
OpenRouter, a local llama.cpp server, or any compatible proxy — no vendor binary
on the host.

```json
{ "engine": { "per_role": { "ba": { "engine": "http", "model": "gpt-4o-mini" } } } }
```

How it runs, by role:

- **Text roles** (BA/PO/SM/SA planning, chat replies, review, docs) get a single
  chat completion — the reply is the result.
- **Coding roles** (DEV-BUG/DEV-FEATURE) get an agentic **tool loop**: the model
  inspects and edits the workspace through `read_file` / `list_dir` /
  `write_file` / `done`, up to 12 turns, with every path confined to the
  workspace (absolute paths and `..` are rejected). It writes **full-file**
  content, so it is best suited to focused, few-file edits; for large multi-file
  refactors a CLI engine (`claude`/`opencode`) or `harxes` is still the stronger
  choice. The run's transcript is kept in the activity trace.

## SSO / OIDC (optional)

CoXAgent speaks standard OpenID Connect (authorization-code + PKCE). It is
**off by default**: with none of the variables below set, `/api/auth/oidc/login`
and `/api/auth/oidc/callback` return `404` and only password login is offered,
so an existing deployment is unchanged until an operator turns SSO on.

| Var | Required | Purpose |
|---|---|---|
| `COXAGENT_OIDC_ISSUER` | yes | IdP issuer URL; endpoints + JWKS are auto-discovered from `<issuer>/.well-known/openid-configuration` |
| `COXAGENT_OIDC_CLIENT_ID` | yes | OAuth client id registered with the IdP |
| `COXAGENT_OIDC_CLIENT_SECRET` | yes | OAuth client secret (keep it in `.env`, never commit) |
| `COXAGENT_OIDC_REDIRECT_URI` | yes | Must exactly match the IdP registration, e.g. `https://<domain>/api/auth/oidc/callback` |
| `COXAGENT_OIDC_GROUP_CLAIM` | no | id-token claim carrying group names (default `groups`) |

Setting all four required vars enables SSO. Point your IdP's redirect/callback
at `…/api/auth/oidc/callback` and send users to `…/api/auth/oidc/login`.

**Flow.** `login` mints `state`/`nonce`/PKCE and redirects to the IdP;
`callback` validates the one-time `state` (unknown or older than 10 min ⇒ `400`),
exchanges the code with the PKCE verifier, and verifies the id-token signature
(RS256 against the IdP JWKS) plus `iss`/`aud`/`exp`/`nbf`/`nonce` — any failure
⇒ `401`. On success the user is provisioned/refreshed just-in-time and a normal
hub session cookie is issued.

**Role mapping.** The user's role comes from the group claim: each group label
is matched leniently (case- and punctuation-insensitive, e.g. `tech.lead` →
TechLead, `Super Admin` → Super) against the hub role names, and the
**most-privileged** matching group wins. A user with no recognised group gets
`Viewer` (least privilege). Username is the id-token `email`, falling back to
`sub`.

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

## High availability & failover

CoXAgent runs active/standby with **no manual failover step** — the mechanics
are already in the product; this is how to deploy and reason about them.

**State is external.** Postgres holds the authoritative project state, accounts,
and audit trail; Redis holds coordination + the `cox:events` bus; S3/Mongo hold
blobs/docs. No node holds authoritative state, so **any node can be lost without
data loss** (RPO 0 for committed state — back Postgres up per *Local backing
services* above; Redis is TTL data, safe to lose).

**Control plane is stateless — scale it horizontally.** Run two or more full
nodes (`cox-all`, or the split role binaries) against the *same* managed
Postgres + Redis, behind a load balancer health-checking `GET /api/ready` (live
readiness) / `GET /api/health`. Standby nodes serve reads immediately; there is
no cold spare to promote. `hub.replicas > 1` is valid only with the Redis bus
(the Helm chart enforces this), since cross-node events flow over `cox:events`.

**Single-writer ceremonies use a Redis lease — automatic, split-brain-free.**
Work that must happen exactly once (advancing the project cycle counter,
scrum/BA ceremonies) is gated by an atomic, single-winner leader lease at
`cox:<project>:leader` with a **90 s TTL**. Exactly one node holds it at a time;
if that node dies, the lease expires and another node acquires it on its next
cycle — **leader failover within ~90 s**, no coordination service beyond Redis,
no split brain (the lease is a compare-and-set, not an advisory flag).

**Per-ticket work distributes across runner nodes** via stage leases
(`cox:<project>:stage:<ticket>:<stage>`, **30 min TTL**): multiple runners pick
up different tickets without collision, and a crashed runner's in-flight ticket
is reclaimed when its stage lease expires (or sooner — a restarting hub runs
orphaned-claim recovery). The live roster is worker-presence keys (10 min TTL).

**Per-node self-healing** (single-box deployments) is covered by the
host-watchdog (every 5 min) + autoheal LaunchAgents — see *Local backing
services* and `deploy/install-*.sh`. In a multi-node cluster the LB + lease
handle node loss; the watchdog is belt-and-suspenders for a box that stays up
but wedges.

**Recovery objectives**: leader failover ≤ 90 s; in-flight ticket reclaim
≤ 30 min (usually seconds, on hub restart); reads stay available as long as one
node + Postgres + Redis are up. Runner nodes additionally need their engine CLIs
(`claude`/`opencode`), git and the docker CLI on-image — set `runner.enabled`
per node so only equipped nodes run the execution plane.

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
