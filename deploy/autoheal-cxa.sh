#!/usr/bin/env bash
# autoheal-cxa.sh — Watchdog for the live CXA backend stack (Postgres 5433 + Redis 6379).
# If either DB or Redis goes unresponsive, restart the compose stack.
# Run detached: nohup bash deploy/autoheal-cxa.sh >> autoheal.log 2>&1 &
set -u

DEPLOY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$DEPLOY_DIR/.env"
COMPOSE_FILE="$DEPLOY_DIR/docker-compose.cxa.yml"
PROJECT="cxa-backend"
DB="cxa-backend-db-1"
REDIS="cxa-backend-redis-1"
LOG="$DEPLOY_DIR/autoheal.log"

log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*" | tee -a "$LOG"; }

check() {
  # DB liveness
  if ! docker exec "$DB" pg_isready -U coxagent >/dev/null 2>&1; then
    return 1
  fi
  # Redis liveness (extract password from .env if present)
  local rp
  rp=$(grep -E '^REDIS_PASSWORD=' "$ENV_FILE" 2>/dev/null | head -1 | cut -d= -f2-)
  # Password via env (REDISCLI_AUTH), never argv — argv leaks to `ps` on the host.
  if ! docker exec -e REDISCLI_AUTH="$rp" "$REDIS" redis-cli ping 2>/dev/null | grep -q PONG; then
    return 1
  fi
  return 0
}
    return 1
  fi
  return 0
}

log "autoheal watchdog started (pid $$)"

while true; do
  if ! check; then
    log "ALERT: DB/Redis down or unresponsive — restarting stack"
    if cd "$DEPLOY_DIR" && set -a && . "$ENV_FILE" && set +a; then
      # Graceful stop first (SIGTERM, 30s grace) so Postgres/Redis flush and
      # exit clean instead of `up -d` recreating containers out from under a
      # still-running (if wedged) process. Timeout-bound: a hung stop must not
      # block the restart it's meant to enable.
      if ! timeout 45 docker compose -f "$COMPOSE_FILE" stop -t 30; then
        log "WARN: graceful stop timed out or failed — continuing to up -d anyway"
      fi
      if docker compose -f "$COMPOSE_FILE" up -d; then
        log "OK: stack restart issued"
      else
        log "ERROR: compose up failed — will retry"
      fi
    fi
    # give the stack time to come back after a restart
    sleep 30
  fi
  sleep 60
done
