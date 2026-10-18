FOLDER: Deployment
# Compose Deploy Missing PG_PASSWORD (CXA-B010)

**Keywords:** docker compose, PG_PASSWORD, POSTGRES_PASSWORD, interpolation, required variable missing a value, ${VAR:?}, .env.example, deploy failing, cox-infra

## Overview

CXA-B010 was an incident report: "Deploy failing: docker compose failed: error while interpolating services.db.environment.POSTGRES_PASSWORD: required variable PG_PASSWORD is missing a value: PG_PASSWORD is required - set it before running docker compose up". It fires when `docker compose up` runs against a stack whose Postgres service reads `POSTGRES_PASSWORD` from `${PG_PASSWORD}` and that variable is unset in the shell or `.env`. This page is for anyone who sees this error or edits any of the repo's compose files.

## How it works

Compose interpolates environment references like `${PG_PASSWORD}` during its config-resolution phase (the "interpolate" step named in the error). The project deliberately requires credentials using Compose's modifier forms:

- A bare `${PG_PASSWORD}` resolves to an empty string if unset - Postgres then boots with no password.
- `${PG_PASSWORD:-literal}` silently uses `literal` when unset - a known credential ships by default.
- `${PG_PASSWORD:?message}` - the form this repo requires for credentials - fails interpolation immediately if unset, printing exactly `error while interpolating <yaml-path>: required variable <VAR> is missing a value: <message>`.

That last form produces exactly the CXA-B010 text. Which file fired depends on which stack was brought up; the wording after "required variable" identifies it:

| Exact message suffix | File + line |
|---|---|
| "PG_PASSWORD is required - set it before running docker compose up" | root [`docker-compose.yml`](docker-compose.yml) lines 19 & 35 |
| "Postgres superuser secret must be set" | [`deploy/docker-compose.yml`](deploy/docker-compose.yml) lines 35 & 87; [`deploy/docker-compose.split.yml`](deploy/docker-compose.split.yml) lines 28 & 34 & 80 |
| "Postgres role must be set" (`${PG_USER}`) | same two deploy files (split lines 28 & 34 & 79-85; one-shot line 87) |
| "PG_USER/PG_PASSWORD is required - copy .env.example to .env and fill it in" | [`deploy/local-infra/docker-compose.yml`](deploy/local-infra/docker-compose.yml) lines 20-21 |

Interpolation draws variables from three sources in precedence order (first match wins): exported process environment -> `.env` beside/composed for that file -> in-file literal defaults. The variable must exist somewhere earlier in that chain for interpolation to succeed.

The security gate [`compose_security_gate.rs::insecure_secret_reason`](crates/app/tests/compose_security_gate.rs) enforces this programmatically (COX-C012 / CXA-B029): every credential key must carry the required-marker `${VAR:?msg}`, never a silent fallback or bare reference. So this failure mode is intended guardrail behaviour before any container starts - not incidental breakage.

## Usage

Fix by providing `PG_PASSWORD`. Export it for one-off runs against local backing services:

```sh
export PG_USER=coxagent
export PG_PASSWORD='a-long-random-secret'
docker compose -f deploy/local-infra/docker-compose.yml up -d
```

Or (recommended) copy and source an ignored `.env`, so secrets never enter shell history:

```sh
cp deploy/local-infra/.env.example deploy/local-infra/.env   # backing-services template
# edit deploy/local-infra/.env -> real values (never commit; gitignored)
source deploy/local-infra/.env
docker compose -f deploy/local-infra/docker-compose.yml up -d
```

Root web-app stack on port 8101:

```sh
export PG_PASSWORD='a-long-random-secret'
export COXAGENT_ADMIN_PASSWORD='another-long-random-secret'
docker compose up -d --build        # opens http://localhost:8101 logged in as root (hardcoded)
```

Verify your fix without launching containers by dry-running config resolution:

```sh
source deploy/local-infra/.env                                            # real values now exported
docker compose -f deploy/local-infra/docker-compose.yml config >/dev/null && echo OK   # OK = vars resolve
unset PG_USER PG_PASSWORD                                                 # drop them again:
docker compose -f deploy/local-infra/docker-compose.yml config            # interpolation error reappears here before any container starts
```

(`config` exercises exactly the same interpolation pass as `up`, but creates nothing.)

## Interface

Required variables per entry point (all use `${VAR:?...}`, so omission fails fast with no containers started):

- **Root [`docker-compose.yml`](docker-compose.yml)** (`name:` = dir): requires `PG_PASSWORD` and `COXAGENT_ADMIN_PASSWORD`. The admin login user is hardcoded to `root`.
- **[`deploy/local-infra/docker-compose.yml`](deploy/local-infra/docker-compose.yml)** (`name: cox-infra`, local backing services): requires `PG_USER`, `PG_PASSWORD`, plus MinIO vars only if MinIO is used.
- **[`deploy/docker-compose.cxa.yml`](deploy/docker-compose.cxa.yml)** (`name: cxa-backend`, native hub backend on host ports 5433/6379): references `${PG_USER}`, `${PG_PASSWORD}`, `${REDIS_PASSWORD}` - note these are *bare* (no `:?`) and resolve to empty strings if unset, so this file does NOT fail fast; guard it in your own env.
- **[`deploy/docker-compose.split.yml`](deploy/docker-compose.split.yml)** and the one-shot [`deploy/docker-compose.yml`](deploy/docker-compose.yml) require many secrets incl. `DOMAIN/PUBLIC_IP/ADMIN_*/S3_*/MONGO_*/TURN_SECRET/REDIS_*`.

The canonical listing of every var lives at [`DEPLOYMENT.md - Environment variables`](/DEPLOYMENT.md).

## Configuration

Secret precedence feeding interpolation (first match wins):

| Source | Order | Notes |
|---|---|---|
| Exported process environment | highest | overrides everything else for that run |
| `.env` file beside the compose file (or via flag / COMPOSE_FILE) | middle | gitignored; template has no defaults |
| In-file literal / fallback defaults within YAML | lowest | deliberately absent for credentials |

Credential keys must use `${VAR:?msg}` - enforced by [`compose_security_gate.rs::insecure_secret_reason`]. If you genuinely need an optional credential you may add it to an explicit allow-list in that test, but doing so reintroduces silent blank-password boot. Never weaken Postgres/Redis/Mongo/MinIO/TURN creds just to silence CI.

## Edge cases and limits

It deliberately does NOT cover:

- **Runtime failures after startup** - once variables resolve and containers start, DB auth errors surface elsewhere.
- **Empty-but-set**: exporting an empty string (`export PG_PASSWORD=''`) still satisfies interpolation (the var *is* defined, so `${VAR:?...}` does not fire), so compose proceeds; broken auth then appears at connect time instead of up front.

## Code map

- [`docker-compose.yml`](docker-compose.yml) - root self-host stack (web app on 8101); the exact CXA-B010 message originates here at lines 19 & 35.
- [`deploy/local-infra/docker-compose.yml`](deploy/local-infra/docker-compose.yml) - local backing services (project `cox-infra`) with its own required vars at lines 20-21 & 42-43.
- [`deploy/local-infra/.env.example`](deploy/local-infra/.env.example) - committed template for that stack's `.env`.
- [`deploy/.env.example`](deploy/.env.example) - committed template for split/one-shot deploys.
- [`deploy/docker-compose.cxa.yml`](deploy/docker-compose.cxa.yml) - standalone native-hub backend (project `cxa-backend`) publishing 5433/6379; bare `${...}` refs.
- [`deploy/docker-compose.split.yml`](deploy/docker-compose.split.yml) - full 4-service split deploy; `${PG_PASSWORD}` also feeds `COXAGENT_DB_DSN` / `COXAGENT_AUTH_DSN`.
- [`deploy/docker-compose.yml`](deploy/docker-compose.yml) - one-shot self-host deploy (app + MinIO + Mongo + Postgres + Redis + coturn + Caddy); `${PG_PASSWORD}` feeds the DSN too.
- [`crates/app/tests/compose_security_gate.rs`](crates/app/tests/compose_security_gate.rs) - COX-C012 gate enforcing `${VAR:?msg}` on credentials and loopback datastore bindings; the test that guards this failure mode.
- [`DEPLOYMENT.md`](/DEPLOYMENT.md) - canonical deployment docs: shapes, env list, `.env` hygiene.

## Related

- [CXA-B001 Docker Compose Deploy Failure](cxa-b001-deploy-failing.md) - sibling incident page on the app-driven `docker compose failed:` surface, `host_port`, and compose project-name collisions; shares these same compose files.
- CXA-B029 / COX-C012 security gate (`compose_security_gate.rs`) - the guard that forces `${VAR:?msg}`, whose behaviour this ticket documents.
- COX-B030 (`.env` hygiene) - why `.env.example` is committed but `.env` never is.

