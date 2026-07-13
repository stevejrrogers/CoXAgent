#!/bin/sh
# Ensure a default project + registry exist on the workspace volume, then run
# the hub. State/audit go to Postgres when COXAGENT_DB_DSN is set.
set -e

WS="${COXAGENT_WORKSPACE:-/workspace}"
PORT="${PORT:-4000}"
REG="$WS/registry.json"

mkdir -p "$WS/default/state" "$WS/default/codebase"

if [ ! -f "$REG" ]; then
  echo "first run: onboarding a default project"
  coxagent --state-dir "$WS/default/state" onboard --name "My Project" --alias MYP || true
  printf '[{"id":"default","path":"%s/default"}]\n' "$WS" > "$REG"
fi

exec coxagent hub --registry "$REG" --port "$PORT"
