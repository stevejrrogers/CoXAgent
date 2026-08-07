#!/bin/sh
# Boot a locally built coxagent WITH RBAC enabled for authenticated e2e.
# Uses its own throwaway state copy on its own port (never hub 4000).
set -e
PORT="${1:-4518}"
HERE="$(cd "$(dirname "$0")" && pwd)"
# Nest the throwaway state under .state-auth/serve so build_auth()'s base —
# which is derived from `--state-dir ..`'s PARENT — resolves to .state-auth/
# and NOT to $HERE itself. That keeps bootstrap_admin()'s account file out of
# $HERE/auth.json, which would otherwise switch ON RBAC for the sibling open
# (non-AUTH) playwright config on any machine that runs both suites.
STATE="$HERE/.state-auth/serve"
rm -rf "$STATE"
mkdir -p "$STATE"
cp -R "$HERE/fixtures/state/." "$STATE/"
BIN="$HERE/../target/debug/coxagent"
[ -x "$BIN" ] || { echo "build first: cargo build --bin coxagent" >&2; exit 1; }
# Keep this suite hermetic too: the auth store must be the throwaway JSON
# account file under .state-auth/serve, never the ambient live Postgres DSN a
# dev shell may export. Without this, build_auth() connects to shared state.
unset COXAGENT_DB_DSN COXAGENT_AUTH_DSN COXAGENT_REDIS_URL COXAGENT_REMOTE_STORE_URL 2>/dev/null || true
export COXAGENT_PORT="$PORT"
export COXAGENT_ADMIN_USER="${COXAGENT_ADMIN_USER:-adminos}"
export COXAGENT_ADMIN_PASSWORD="${COXAGENT_ADMIN_PASSWORD:-ChangeMe_12345}"
exec "$BIN" --state-dir "$STATE" serve --work-dir "$HERE/.."
