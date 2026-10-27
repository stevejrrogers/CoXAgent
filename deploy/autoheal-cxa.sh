#!/usr/bin/env bash
# autoheal-cxa.sh — Watchdog for the live CXA backend stack (Postgres 5433 + Redis 6379).
# If either DB or Redis goes unresponsive, restart the compose stack.
# Run detached: nohup bash deploy/autoheal-cxa.sh >> autoheal.log 2>&1 &
#
# Secrets (PG_USER/PG_PASSWORD/REDIS_PASSWORD) come from deploy/.env when it
# exists (classic deploy topology); otherwise they are derived from the host's
# canonical ~/CoXAgent/coordination.json — the SAME source host-watchdog.sh
# reads — so a host that runs the native hub without a deploy/.env is still
# covered. Secrets only ever pass through the environment, never argv (argv
# leaks to `ps`).
#
# A real outage must persist across several consecutive checks before the
# stack is bounced: a needless `compose stop` is a full DB outage for the whole
# team, so a single transient blip (a slow pg_isready, a GC pause) must never
# trigger a restart.
set -u
# launchd (KeepAlive, install-autoheal.sh) runs us with a minimal PATH that
# omits Homebrew and Docker's CLI dir, so bare `docker` was "command not found"
# and the stack-restart silently no-op'd (observed 2026-10-27). Prepend the
# usual locations so `docker` is found however we are launched.
export PATH="/opt/homebrew/bin:/usr/local/bin:/Applications/Docker.app/Contents/Resources/bin:$PATH"

DEPLOY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$DEPLOY_DIR/.env"
COORD="${COXAGENT_COORD:-$HOME/CoXAgent/coordination.json}"
COMPOSE_FILE="$DEPLOY_DIR/docker-compose.cxa.yml"
PROJECT="cxa-backend"
DB="cxa-backend-db-1"
REDIS="cxa-backend-redis-1"
LOG="$DEPLOY_DIR/autoheal.log"
FAILS_BEFORE_RESTART="${AUTOHEAL_FAILS_BEFORE_RESTART:-3}"
RECHECK_GAP="${AUTOHEAL_RECHECK_GAP:-10}"

log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*" | tee -a "$LOG"; }

# Populate PG_USER/PG_PASSWORD/REDIS_PASSWORD in the environment. Returns
# non-zero if the datastore secrets could not be resolved (so the caller does
# not attempt a credential-less restart, which would recreate the stack with
# blank passwords — the CXA-B016 hazard the compose file fails fast on).
load_secrets() {
  if [ -f "$ENV_FILE" ]; then
    set -a; . "$ENV_FILE"; set +a
  elif [ -f "$COORD" ]; then
    eval "$(/usr/bin/python3 - "$COORD" <<'PY'
import json, re, sys, shlex
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)
def userpass(dsn):
    m = re.search(r'://([^:@/]*):([^@]*)@', dsn or '')
    return (m.group(1), m.group(2)) if m else ('', '')
pu, pp = userpass(d.get('db_dsn', ''))
_,  rp = userpass(d.get('redis_url', ''))   # redis_url is redis://:PW@host
for k, v in (('PG_USER', pu), ('PG_PASSWORD', pp), ('REDIS_PASSWORD', rp)):
    if v:
        print(f'export {k}={shlex.quote(v)}')
PY
)"
  fi
  [ -n "${REDIS_PASSWORD:-}" ] && [ -n "${PG_PASSWORD:-}" ] && [ -n "${PG_USER:-}" ]
}

check() {
  # DB liveness
  if ! docker exec "$DB" pg_isready -U "${PG_USER:-coxagent}" >/dev/null 2>&1; then
    return 1
  fi
  # Redis liveness — password via env (REDISCLI_AUTH), never argv.
  if ! docker exec -e REDISCLI_AUTH="${REDIS_PASSWORD:-}" "$REDIS" \
         redis-cli ping 2>/dev/null | grep -q PONG; then
    return 1
  fi
  return 0
}

if ! load_secrets; then
  log "FATAL: could not resolve datastore secrets from $ENV_FILE or $COORD — refusing to run (a credential-less restart would blank the stack)"
  exit 1
fi

log "autoheal watchdog started (pid $$, needs $FAILS_BEFORE_RESTART consecutive failures to act)"

while true; do
  if ! check; then
    # Confirm a real outage across consecutive checks before bouncing the stack.
    fails=1
    while [ "$fails" -lt "$FAILS_BEFORE_RESTART" ]; do
      sleep "$RECHECK_GAP"
      if check; then break; fi
      fails=$((fails + 1))
    done
    if [ "$fails" -ge "$FAILS_BEFORE_RESTART" ]; then
      log "ALERT: DB/Redis unresponsive on $fails consecutive checks — restarting stack"
      if load_secrets; then
        # Graceful stop first (SIGTERM, 30s grace) so Postgres/Redis flush and
        # exit clean instead of `up -d` recreating containers out from under a
        # still-running (if wedged) process. Timeout-bound: a hung stop must
        # not block the restart it is meant to enable.
        if ! timeout 45 docker compose -f "$COMPOSE_FILE" stop -t 30; then
          log "WARN: graceful stop timed out or failed — continuing to up -d anyway"
        fi
        if (cd "$DEPLOY_DIR" && docker compose -f "$COMPOSE_FILE" up -d); then
          log "OK: stack restart issued"
        else
          log "ERROR: compose up failed — will retry"
        fi
      else
        log "ERROR: could not resolve secrets for restart — will retry"
      fi
      # give the stack time to come back, then verify liveness again.
      sleep 30
      if check; then
        log "OK: DB/Redis healthy again after restart"
      else
        log "WARN: DB/Redis still down after restart"
      fi
    else
      log "transient DB/Redis blip cleared after re-check — no restart"
    fi
  fi
  sleep 60
done
