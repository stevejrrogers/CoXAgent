FOLDER: Deployment
# Standalone CXA Backend Stack (CXA-F026)

**Keywords:** cxa-backend, docker compose, Postgres 5433, Redis 6379, coordination.json, source_cxa_data, coxagent_db named volume, native hub backend, autoheal-cxa.sh

## Overview

CXA-F026 provides the file deploy/docker-compose.cxa.yml - a dedicated Docker Compose stack that runs only the native CXA hub's own backend: Postgres on host loopback port 5433 and Redis on host loopback port 6379, under the compose project name cxa-backend.

The repo's root docker-compose.yml and its split deploy both name their database service db and their project coxagent, so they collide with CoXAgent's own running containers; this stack keeps them separate.

## How it works

The manifest declares two services under compose project cxa-backend, each publishing to loopback only.

Service db - image postgres:16-alpine, publishes host loopback port 5433 mapped to container 5432. It reads credentials from PG_USER, PG_PASSWORD and POSTGRES_DB, defines a healthcheck (pg_isready -U ${PG_USER}, 5s interval, 3s timeout, 10 retries), sets restart to unless-stopped, and mounts an external named volume source_cxa_data that resolves to coxagent_db. Because the volume is external and already carries CXA's data, Postgres attaches to the existing store instead of creating an empty one.

Service redis - image redis:7-alpine, publishes host loopback port 6379. It starts with persistence disabled (--save "" --appendonly no) and requires a password passed as a server argument from REDIS_PASSWORD; an unauthenticated Redis would be an auth bypass since it holds live sessions.

Per the manifest's header comment, the native hub reads ~/CoXAgent/coordination.json, which points Postgres and Redis at these two published ports so the stack satisfies exactly what the hub expects.

Two supporting files keep it alive:

- deploy/autoheal-cxa.sh - a bash watchdog that polls both containers every 60s using docker exec pg_isready -U coxagent and then redis-cli ping (password supplied through env auth only); if either fails it sources deploy/.env and re-runs docker compose -f deploy/docker-compose.cxa.yml up -d.
- deploy/.env.example - documents every interpolated secret; the live deploy/.env you create from it holds real credentials and is gitignored (COX-B030 guard).

## Usage

Bring up only the native CXA backend without colliding with any other Compose stack on this host:

```sh
cd deploy
cp .env.example .env            # fill PG_USER / PG_PASSWORD / REDIS_PASSWORD ...
set -a; source .env; set +a     # export so compose interpolation sees them
docker compose -f docker-compose.cxa.yml up -d
```

Start the watchdog against your existing data (no fresh empty volume is created):

```sh
nohup bash deploy/autoheal-cxa.sh >> deploy/autoheal.log 2>&1 &
```

Tear down when done while preserving data (the external volume survives):

```sh
cd deploy && docker compose -f docker-compose.cxa.yml down   # containers stopped,
                                                             # coxagent_db kept
```

## Interface

Compose manifest fields in deploy/docker-compose.cxa.yml:

- top-level name: cxa-backend
- services.db.image: postgres:16-alpine
- services.db publishing: binds Postgres container port 5432 to host loopback port 5433 (host-port:container-port = 5433:5432)
- services.db.environment.POSTGRES_USER set from ${PG_USER}
- services.db.environment.POSTGRES_PASSWORD set from ${PG_PASSWORD}
- services.db.environment.POSTGRES_DB = coxagent
- services.db volumes mount source_cxa_data at /var/lib/postgresql/data
- services.db.healthcheck.test: pg_isready -U ${PG_USER} with 5s interval / 3s timeout / 10 retries
- services.db.restart: unless-stopped
- services.redis.image: redis:7-alpine
- services.redis publishing: binds Redis container port 6379 to host loopback port 6379 (6379:6379)
- services.redis.command: redis-server --save "" --appendonly no --requirepass ${REDIS_PASSWORD}
- volumes.source_cxa_data.external.name = coxagent_db (the pre-existing data volume)

## Configuration

Every setting is driven by compose interpolation from exported environment or a deploy/.env file beside the manifest:

- PG_USER - Postgres role used both to create the superuser and in the healthcheck. No default; required.
- PG_PASSWORD - Postgres password. No default; required.
- REDIS_PASSWORD - Redis requirepass secret; never blank (unauthenticated Redis would be an auth bypass). Required.

The manifest itself hard-codes POSTGRES_DB as coxagent, the volume external name coxagent_db, both images/tags, the healthcheck timings, restart policy and redis persistence-off flags - none of these are configurable.

The watchdog autoheal-cxa.sh reads its own env source (deploy/.env) and hard-codes polling/sleep intervals plus container names cxa-backend-db-1 and cxa-backend-redis-1 in its source.

## Edge cases and limits

- Missing credentials: if PG_USER / PG_PASSWORD / REDIS_PASSWORD are unset (or the deploy/.env is absent), compose interpolation fails up and the stack never starts - it does not fall back to defaults or empty passwords.
- Empty-volume trap: because source_cxa_data resolves to the external volume coxagent_db, an up against a host that has never created coxagent_db will not implicitly provision data; ensure the volume exists with real data before relying on it. The manifest's comment explicitly warns "do NOT create a new empty one."
- Host port clash: binding 5433/6379 on loopback fails if anything else already owns those host ports; bring the colliding stack down first (see Related for the collision this avoids).
- No persistence for Redis by design: sessions rely on native TTL, so a container restart clears them - expected behaviour, not data loss.
- It deliberately does NOT run Mongo or S3/MinIO from here; those belong to other stacks covered in Related pages.

## Code map

The whole feature is one small manifest plus one watchdog script:

- deploy/docker-compose.cxa.yml - the standalone backend stack itself (name cxa-backend, db postgres:16-alpine, redis redis:7-alpine, external volume coxagent_db). This is THE file for CXA-F026.
- deploy/autoheal-cxa.sh - bash watchdog that restarts this stack when either datastore goes unresponsive.
- deploy/.env.example - template documenting every interpolated secret this stack needs; copy to deploy/.env.

## Related

- docs/wiki/engineering/deployment/cxa-b001-docker-compose-deploy-failure.md - documents the compose service/project-name collision between CoXAgent's own containers and web/split deploys that this stack was written to fix; covers host_port assignment and verify_deploy_health.
- docs/wiki/engineering/deployment/cxa-b010-pg-password-missing.md - explains Compose credential interpolation and why missing-variable failures are intended guardrail behaviour via ${VAR:?} markers.
