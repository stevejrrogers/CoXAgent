#!/bin/sh
# Boot the locally built coxagent against a throwaway copy of the frozen
# fixture state. COXAGENT_PORT keeps dogfood builds off the hub's port 4000.
set -e
PORT="${1:-4517}"
HERE="$(cd "$(dirname "$0")" && pwd)"
# Nest the throwaway state under .state/serve, the way run-server-auth.sh
# does: the auth account file is written to the state dir's PARENT, so a flat
# `--state-dir $HERE/.state` leaves an auth.json in $HERE — OUTSIDE the dir
# this script wipes. That file survives every later run and switches RBAC on
# for a suite whose specs are all unauthenticated, turning them into login-wall
# timeouts until someone deletes it by hand. Nesting keeps the parent inside
# the wiped dir, so each run really does start with no accounts.
STATE="$HERE/.state/serve"
rm -rf "$HERE/.state"
# Residue from the flat layout above: on any machine that ran the old script,
# $HERE/auth.json is still there and still turns RBAC on. It is throwaway
# fixture state (the auth suite keeps its own under .state-auth/), so clear it
# instead of asking every operator to remember.
rm -f "$HERE/auth.json"
mkdir -p "$STATE"
cp -R "$HERE/fixtures/state/." "$STATE/"
BIN="$HERE/../target/debug/coxagent"
[ -x "$BIN" ] || { echo "build first: cargo build --bin coxagent" >&2; exit 1; }
# Keep the suite hermetic: a shell that exports the live hub's Postgres/Redis
# DSNs would otherwise make this throwaway fixture boot point at shared state
# (RBAC on, real users) instead of the isolated JSON store — which silently
# flips every unauthenticated spec to a login-wall timeout.
unset COXAGENT_DB_DSN COXAGENT_AUTH_DSN COXAGENT_REDIS_URL COXAGENT_REMOTE_STORE_URL 2>/dev/null || true
# Same reason, one layer up: an exported admin user/password bootstraps an
# account on boot, which is RBAC on, which is the login wall again. The auth
# suite sets these itself (run-server-auth.sh); this one must not inherit them.
unset COXAGENT_ADMIN_USER COXAGENT_ADMIN_PASSWORD 2>/dev/null || true
exec env COXAGENT_PORT="$PORT" "$BIN" --state-dir "$STATE" serve --work-dir "$HERE/.."
