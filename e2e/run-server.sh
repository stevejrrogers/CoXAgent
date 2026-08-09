#!/bin/sh
# Boot the locally built coxagent against a throwaway copy of the frozen
# fixture state. COXAGENT_PORT keeps dogfood builds off the hub's port 4000.
set -e
PORT="${1:-4517}"
HERE="$(cd "$(dirname "$0")" && pwd)"
STATE="$HERE/.state"
rm -rf "$STATE"
mkdir -p "$STATE"
cp -R "$HERE/fixtures/state/." "$STATE/"
BIN="$HERE/../target/debug/coxagent"
[ -x "$BIN" ] || { echo "build first: cargo build --bin coxagent" >&2; exit 1; }
# Keep the suite hermetic: a shell that exports the live hub's Postgres/Redis
# DSNs would otherwise make this throwaway fixture boot point at shared state
# (RBAC on, real users) instead of the isolated JSON store — which silently
# flips every unauthenticated spec to a login-wall timeout.
unset COXAGENT_DB_DSN COXAGENT_AUTH_DSN COXAGENT_REDIS_URL COXAGENT_REMOTE_STORE_URL 2>/dev/null || true
exec env COXAGENT_PORT="$PORT" "$BIN" --state-dir "$STATE" serve --work-dir "$HERE/.."
