#!/bin/sh
# Boot the locally built coxagent against a throwaway copy of the frozen
# fixture state. COXAGENT_PORT keeps dogfood builds off the hub's port 4000.
set -e
PORT="${1:-4517}"
HERE="$(cd "$(dirname "$0")" && pwd)"
# Port + identity guard (CXA-F315): attribute-checked eviction of a stale
# fixture server (CXA-B083's classify-before-evict lesson), bounded wait for
# the port to actually release, and proof that what answers the port really
# is this repo's hub before any spec talks to it.
. "$HERE/fixture-guard.sh"

evict_stale_fixture "$PORT"
await_port_free "$PORT"
STATE="$HERE/.state/serve"
# Nest the throwaway state under .state/serve, the way run-server-auth.sh
# does: the auth account file is written to the state dir's PARENT, so a flat
# `--state-dir $HERE/.state` leaves an auth.json in $HERE — OUTSIDE the dir
# this script wipes. That file survives every later run and switches RBAC on
# for a suite whose specs are all unauthenticated, turning them into login-wall
# timeouts until someone deletes it by hand. Nesting keeps the parent inside
# the wiped dir, so each run really does start with no accounts.
rm -rf "$HERE/.state"
# Residue from the flat layout above: on any machine that ran the old script,
# $HERE/auth.json (and the sessions.json the auth store writes beside it) are
# still there and still carry accounts with live bearer sessions. They are
# throwaway fixture state (the auth suite keeps its own under .state-auth/),
# so clear them instead of asking every operator to remember.
rm -f "$HERE/auth.json"
rm -f "$HERE/sessions.json"
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
# And the same hermeticity one listener over (CXA-B151): the metrics admin
# listener defaults to 127.0.0.1:9010, which the live hub on a dev host
# already owns — without a pin the fixture boots WITHOUT its metrics
# endpoint, announced only by one easy-to-miss error line. Pin a kernel-
# assigned free port so the suite's server always has the listener. The pin
# overrides any ambient COXAGENT_METRICS_PORT on purpose (hermetic, like the
# unsets above); the plain assignment makes a pick failure abort the boot.
METRICS_PORT="$(pick_free_loopback_port)"
echo "fixture metrics admin port: $METRICS_PORT" >&2
# The old `exec` made identity checking impossible — once exec'd, nothing of
# the wrapper is left to probe with. Run the server as a child, prove its
# identity, then hold the wrapper open for Playwright's webServer contract.
# If the wrapper is ever SIGKILLed past this trap, the orphan is exactly the
# stale fixture the guard evicts on the next boot — the loop stays closed.
COXAGENT_PORT="$PORT" COXAGENT_METRICS_PORT="$METRICS_PORT" "$BIN" --state-dir "$STATE" serve --work-dir "$HERE/.." &
SERVER_PID=$!
trap 'kill -TERM "$SERVER_PID" 2>/dev/null || true' EXIT INT TERM
await_identity "$PORT" "$SERVER_PID"
wait "$SERVER_PID"
