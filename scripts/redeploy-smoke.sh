#!/usr/bin/env bash
# redeploy-smoke.sh — CXA-B308 end-to-end redeploy regression against the REAL
# root docker-compose.yml (test harness only; no unit test spawns servers).
#
# Three scenarios, one assertion each, exact PASS marker per scenario:
#   1. CLEAN REDEPLOY      — two consecutive deploys succeed: the down/up
#                            sequence releases host port P and reacquires it,
#                            deterministically (CXA-B295: "Bind for 0.0.0.0:P
#                            failed: port is already allocated").
#   2. INTERRUPTED RUN     — a stack left partially up (only the
#                            port-publishing service started OUTSIDE this
#                            script) does not wedge the next full deploy:
#                            down --remove-orphans clears the strays first.
#   3. PORT OVERRIDE       — with the host-port override env set, the deploy
#                            targets the alternate host port and the deploy
#                            log STATES it.
#
# Degrades gracefully where docker is absent/unreachable: prints SKIP per
# scenario and exits 0, so CI runners without docker stay green.
#
# Usage:  scripts/redeploy-smoke.sh            # run all three scenarios
#         DOCKER=absent-docker ./scripts/redeploy-smoke.sh   # force SKIP path
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJ="cxa-redeploy-smoke"

# Throwaway credentials for THIS smoke stack only — never production secrets.
PG_PASSWORD="${SMOKE_PG_PASSWORD:-smoke-local-pg}"
ADMIN_PASSWORD="${SMOKE_ADMIN_PASSWORD:-smoke-local-admin}"

# Smoke runs on its own ports so it can never fight a live hub on 8101
# (see AGENTS.md port reservation note). Overridable for operators.
BASE_PORT="${SMOKE_BASE_PORT:-8144}"
CLEAN_PORT="$BASE_PORT"
INTERRUPTED_PORT="$((BASE_PORT + 1))"
OVERRIDE_PORT="$((BASE_PORT + 2))"

DOCKER="${DOCKER:-docker}"
SEEN_FAIL=0

log()  { printf '%s\n' "$*"; }
pass() { log "PASS: $*"; }
fail() { SEEN_FAIL=1; log "FAIL: $*"; }

# The publish-mapping knob root docker-compose.yml speaks (root compose:
# APP_PORT; the one-shot deploy/ compose: COXAGENT_HOST_PORT — CXA-B306).
PUBLISH_PORT_VAR="${SMOKE_PUBLISH_PORT_VAR:-APP_PORT}"

compose() {
  # NOTE: plain `VAR=value cmd` prefixes work on shell FUNCTIONS; `env VAR=x
  # func` does not (env execs binaries only).
  PG_PASSWORD="$PG_PASSWORD" COXAGENT_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
    "$DOCKER" compose -p "$PROJ" --project-directory "$ROOT" "$@"
}

teardown_stack() {
  compose down -v --remove-orphans >/dev/null 2>&1 || true
}

# Wait until SOMETHING answers HTTP on host:port (any status; connection refused
# is the only failure mode we care about here). Best-effort within timeout.
wait_http_ready() {
  local port="$1" tries="${2:-90}" code
  while (( tries-- > 0 )); do
    code="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:${port}/" 2>/dev/null || true)"
    if [[ -n "$code" && "$code" != "000" ]]; then
      return 0
    fi
    sleep 2
  done
  return 1
}

deploy_full() {
  # $1 = target host port; recorded in the deploy log so every scenario can
  # prove WHICH port this deploy acted on (attribution, not vibes).
  local port="$1"
  log >>"$LOG" "deploy: down --remove-orphans then up -d --build, host port ${port}"
  # The exact bring-up docker-compose.yml documents (README/self-host):
  # down first, then up -d --build — the deploy gate's release-and-reacquire.
  compose down --remove-orphans >>"$LOG" 2>&1 || true
  export "${PUBLISH_PORT_VAR}=$port"; compose up -d --build >>"$LOG" 2>&1
}

deploy_log_states_port() {
  grep -Eq "host port ${1}\b" "$LOG"
}

# Prove compose ACTUALLY published host port $1 -> container 4000 right now
# (`docker compose ps` reports mappings as `0.0.0.0:<port>->4000/tcp`).
published_mapping_is_live() {
  compose ps 2>/dev/null | grep -Eq "${1}->4000"
}

if ! command -v "$DOCKER" >/dev/null 2>&1; then
  log "SKIP: scenario 1 clean redeploy — docker not installed (${DOCKER})"
  log "SKIP: scenario 2 interrupted-stack redeploy — docker not installed (${DOCKER})"
  log "SKIP: scenario 3 port override — docker not installed (${DOCKER})"
  exit 0
fi
if ! "$DOCKER" info >/dev/null 2>&1; then
  log "SKIP: scenario 1 clean redeploy — docker daemon unreachable"
  log "SKIP: scenario 2 interrupted-stack redeploy — docker daemon unreachable"
  log "SKIP: scenario 3 port override — docker daemon unreachable"
  exit 0
fi
if ! command -v curl >/dev/null 2>&1; then
  log "SKIP: scenario 1..3 — curl not installed, readiness probe impossible"
  exit 0
fi

LOG="$(mktemp)"
trap 'rm -f "$LOG"; teardown_stack' EXIT

# --- Scenario 1: CLEAN REDEPLOY -------------------------------------------
( deploy_full "$CLEAN_PORT" && wait_http_ready "$CLEAN_PORT" \
  && deploy_full "$CLEAN_PORT" && wait_http_ready "$CLEAN_PORT" ) \
  && published_mapping_is_live "$CLEAN_PORT"
if (( $? == 0 )); then
  pass "scenario 1 clean redeploy: two consecutive deploys on host port ${CLEAN_PORT}, port released and reacquired both times"
else
  fail "scenario 1 clean redeploy (log tail below)"; tail -30 "$LOG"
fi

# --- Scenario 2: INTERRUPTED PREVIOUS RUN ----------------------------------
: >"$LOG"
teardown_stack
# Simulate a half-finished prior deploy: ONLY the port-publishing service
# brought up OUTSIDE the deploy gate — the state a crashed deploy leaves
# behind (stray containers still holding the host port). Published on ITS OWN
# port so the stray can never collide with anything else on the host.
export "${PUBLISH_PORT_VAR}=${INTERRUPTED_PORT}"; compose up -d --no-build coxagent >>"$LOG" 2>&1
if ! compose ps 2>/dev/null | grep -qE '(^|[[:space:]])coxagent'; then
  fail "scenario 2 setup: could not leave a partial stack up (log tail below)"; tail -20 "$LOG"
elif deploy_full "$INTERRUPTED_PORT" \
  && wait_http_ready "$INTERRUPTED_PORT" \
  && published_mapping_is_live "$INTERRUPTED_PORT"; then
  pass "scenario 2 interrupted previous run: partial coxagent-only stack did not block the full deploy on host port ${INTERRUPTED_PORT} (down removed the orphans)"
else
  fail "scenario 2 interrupted previous run (log tail below)"; tail -30 "$LOG"
fi

# --- Scenario 3: PORT OVERRIDE ---------------------------------------------
: >"$LOG"
teardown_stack
deploy_full "$OVERRIDE_PORT" \
  && wait_http_ready "$OVERRIDE_PORT" \
  && deploy_log_states_port "$OVERRIDE_PORT" \
  && published_mapping_is_live "$OVERRIDE_PORT"
if (( $? == 0 )); then
  pass "scenario 3 port override: ${PUBLISH_PORT_VAR}=${OVERRIDE_PORT} honoured — deploy log states host port ${OVERRIDE_PORT} and the mapping is live"
else
  fail "scenario 3 port override (log tail below)"; tail -30 "$LOG"
fi

exit "$SEEN_FAIL"
