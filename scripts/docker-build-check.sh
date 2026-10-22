#!/usr/bin/env bash
# CXA-B313 pre-deploy gate: prove the compose stack still BUILDS and its config
# interpolates, without ever pointing a human at a docker-desktop:// dashboard.
#
# Usage:   scripts/docker-build-check.sh          # config check + full build
#          scripts/docker-build-check.sh --no-cache   # cold build (CI mode)
# Exit 0 when green; exit non-zero with the REAL tail of the build log on stdout.
set -uo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

NO_CACHE=()
if [ "${1:-}" = "--no-cache" ]; then NO_CACHE=(--no-cache); fi

# `docker compose config` hard-fails on required interpolation vars
# (PG_PASSWORD et al.). The gate must validate the stack exactly as a deploy
# runs it, so fall back to throwaway strong values when none is exported.
export PG_PASSWORD="${PG_PASSWORD:-buildcheck-$(date +%s)-placeholder}"
export COXAGENT_ADMIN_PASSWORD="${COXAGENT_ADMIN_PASSWORD:-$PG_PASSWORD}"

echo "== docker compose config =="
docker compose -f docker-compose.yml config -q \
  || { echo "FAIL: docker compose config rejected docker-compose.yml"; exit 1; }
echo "config OK"

echo "== docker compose build =="
LOG="/tmp/coxagent-build.$$.log"
trap 'rm -f "$LOG"' EXIT
# `compose build` reports per-service failure on stderr but the useful compiler
# output lives in the combined stream — capture everything to $LOG.
# --ssh default: harxes-core is a private ssh git dependency, so cargo must see
# the caller's agent inside BuildKit (the raw `docker compose build` without it
# fails "attempted ssh-agent authentication, but no usernames succeeded").
if ! docker compose -f docker-compose.yml build --progress=plain --ssh default \
     ${NO_CACHE[@]+"${NO_CACHE[@]}"} 2>&1 | tee "$LOG"; then
  echo
  echo "============================================================"
  echo "BUILD FAILED — last 80 lines of real build output (no dashboard link needed):"
  echo "============================================================"
  tail -n 80 "$LOG"
  exit 1
fi

echo "BUILD OK — both services compile and compose config is valid."
exit 0
