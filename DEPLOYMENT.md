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
cp .env.example .env   # fill in domain, admin password, secrets
docker compose up -d
```

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

Create API tokens in Users → API tokens; scope-check happens server-side per
project argument.

## Service roles (until the 4-binary split lands)

| Target service | Run today as |
|---|---|
| cox-gateway | `coxagent hub` + `COXAGENT_NO_INLINE_EXEC=1` (Helm sets it) |
| cox-runner | `coxagent run` (`COXAGENT_OPERATOR` identity) |
| cox-realtime / cox-knowledge | inside the hub process (split scheduled) |

## Upgrade & rollback

- The hub is safe to restart at any time: sessions persist, runners re-attach,
  an interrupted agent run is retried by the normal cycle machinery.
- Rolling upgrade order: runners → hub (contracts are versioned; frames with a
  different `CONTRACT_VERSION` are skipped, not misparsed).

## App distribution & in-app updates

1. Tag a release: `git tag v0.94.0 && git push origin v0.94.0` — the `release`
   workflow builds the macOS `.dmg`, Windows `.exe`, and Linux `.tar.gz` and
   attaches them to the GitHub Release.
2. Point the hub at the repo once: Settings → Workspace → App downloads →
   `releases_repo` (e.g. `stevejrrogers/CoXAgent`). The hub polls the latest
   release every 30 minutes and republishes version + per-platform URLs at
   `GET /api/app/latest`. Manual URL fields override auto-detected assets;
   iOS is always a manual App Store / TestFlight link.
3. Every signed-in client compares the hub's version with the latest release:
   newer → a top banner ("CoXAgent X đã có — Update") opens the Get-CoXAgent
   modal, which highlights the platform the user is on. The download icon next
   to the user badge opens the same modal any time.
