#!/usr/bin/env bash
# Run a local MongoDB for CoXAgent's documentation store, and print the env vars
# to point the app at it. With Mongo configured, the living documentation is the
# system of record on the server (persists independently of project state.json).
#
#   scripts/mongo.sh up       # start MongoDB (port 27017)
#   scripts/mongo.sh down      # stop & remove it
#   scripts/mongo.sh env       # print the COXAGENT_MONGO_* env vars
set -euo pipefail

NAME=coxagent-mongo
PORT="${MONGO_PORT:-27017}"
DB="${MONGO_DB:-coxagent}"

env_block() {
  cat <<EOF
export COXAGENT_MONGO_URL=mongodb://127.0.0.1:$PORT
export COXAGENT_MONGO_DB=$DB
EOF
}

case "${1:-up}" in
  up)
    if docker ps -a --format '{{.Names}}' | grep -qx "$NAME"; then
      docker start "$NAME" >/dev/null
    else
      docker run -d --name "$NAME" \
        -p "$PORT:27017" \
        -v coxagent-mongo:/data/db \
        mongo:7 >/dev/null
    fi
    echo "==> MongoDB running: mongodb://localhost:$PORT (db '$DB')"
    echo "    Point the app at it with:"
    echo
    env_block
    ;;
  down) docker rm -f "$NAME" >/dev/null 2>&1 || true; echo "==> MongoDB stopped." ;;
  env) env_block ;;
  *) echo "usage: $0 {up|down|env}"; exit 1 ;;
esac
