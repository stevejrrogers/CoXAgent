#!/bin/sh
# Boot a locally built coxagent WITH the in-memory ticket archive wired
# (CXA-F274), seeded from the frozen archive fixture, for the archive e2e
# suite. Own throwaway state copy, own port (never hub 4000, never the
# sibling suites' 4517/4518).
set -e
PORT="${1:-4527}"
HERE="$(cd "$(dirname "$0")" && pwd)"
# Port + identity guard (CXA-F315), same contract as run-server.sh.
. "$HERE/fixture-guard.sh"

evict_stale_fixture "$PORT"
await_port_free "$PORT"
STATE="$HERE/.state-archive/serve"
# Same nesting rule as the auth suite: the state dir's PARENT is where
# build_auth() looks, so nesting under .state-archive/ keeps any account file
# out of $HERE — a stray auth.json here would switch RBAC on for the sibling
# open suite. Wipe the whole tree so no run inherits the previous one.
rm -rf "$HERE/.state-archive"
mkdir -p "$STATE"
cp -R "$HERE/fixtures/state/." "$STATE/"
BIN="$HERE/../target/debug/coxagent"
[ -x "$BIN" ] || { echo "build first: cargo build --bin coxagent" >&2; exit 1; }
# Hermetic, like every fixture boot: no ambient shared state.
unset COXAGENT_DB_DSN COXAGENT_AUTH_DSN COXAGENT_REDIS_URL COXAGENT_REMOTE_STORE_URL 2>/dev/null || true
unset COXAGENT_ADMIN_USER COXAGENT_ADMIN_PASSWORD 2>/dev/null || true
# The archive under test: the env-gated in-memory ArchiveStorePort adapter
# (the Mongo cold store of CXA-F272 takes this slot in production), seeded so
# the suite boots with a POPULATED archive instead of an empty one. A seed
# that cannot be read or parsed refuses to boot the archive — a silently
# empty one would turn every assertion here into a false pass.
COXAGENT_ARCHIVE_MEMORY=1
COXAGENT_ARCHIVE_MEMORY_SEED="$HERE/fixtures/archive-seed.json"
export COXAGENT_ARCHIVE_MEMORY COXAGENT_ARCHIVE_MEMORY_SEED
COXAGENT_PORT="$PORT"
COXAGENT_METRICS_PORT="$(pick_free_loopback_port)"
export COXAGENT_PORT COXAGENT_METRICS_PORT
echo "fixture metrics admin port: $COXAGENT_METRICS_PORT" >&2
# Child + identity proof + hold-open wrapper (see run-server.sh): the EXIT
# trap hands Playwright's TERM down to the server.
"$BIN" --state-dir "$STATE" serve --work-dir "$HERE/.." &
SERVER_PID=$!
trap 'kill -TERM "$SERVER_PID" 2>/dev/null || true' EXIT INT TERM
await_identity "$PORT" "$SERVER_PID"
wait "$SERVER_PID"
