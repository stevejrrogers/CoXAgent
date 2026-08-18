#!/usr/bin/env bash
# deploy-cxa.sh — build -> swap -> codesign -> restart -> verify the CoXAgent hub.
#
# Used by the overnight autonomous agent to roll out a code fix quickly and
# deterministically. Run from the deploy worktree root (/private/tmp/coxa-agent-main).
#
# Usage:
#   ./deploy/deploy-cxa.sh <worktree_root> [--restart]
#
# NOTE: NEVER bind port 4000 here. The hub is started by the desktop app shell;
# we only TERM the existing hub PID and let the shell respawn it. Killing the
# old hub PID is optional (--restart); if you only need a windows-based swap,
# the hub process uses the already-mapped binary only after restart.
set -euo pipefail

WORKTREE="${1:?worktree root required}"
BUNDLE_BIN="/Users/luton/Projects/CoXAgent/desktop/build/CoXAgent.app/Contents/MacOS/cox-server"
LOG="/Users/luton/Projects/CoXAgent/deploy/deploy.log"

log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*" | tee -a "$LOG"; }

# 1. Build release binary from the worktree (already contains the code).
log "[deploy] building release in $WORKTREE"
(cd "$WORKTREE" && cargo build --release --bin coxagent) 2>&1 | tail -5
SRC_BIN="$WORKTREE/target/release/coxagent"
[ -f "$SRC_BIN" ] || { log "[deploy] FAIL: build produced no binary"; exit 1; }

# 2. Backup + swap + codesign.
BK="cox-server.bak.pre-deploy-$(date +%s)"
log "[deploy] backing up to $BK"
cp -X "$BUNDLE_BIN" "/Users/luton/Projects/CoXAgent/desktop/build/CoXAgent.app/Contents/MacOS/$BK"
log "[deploy] swapping binary"
cp -X "$SRC_BIN" "$BUNDLE_BIN"
chmod +x "$BUNDLE_BIN"
log "[deploy] codesigning"
codesign --force --deep --sign - "$BUNDLE_BIN"

# 3. Optionally restart the hub.
if [ "${2:-}" = "--restart" ]; then
  log "[deploy] restarting hub"
  OLD=$(pgrep -f 'cox-server hub' | head -1 || true)
  [ -n "$OLD" ] && kill -TERM "$OLD" 2>/dev/null || true
  for i in $(seq 1 15); do
    sleep 2
    HUB=$(pgrep -f 'cox-server hub' | head -1 || true)
    if [ -n "$HUB" ] && curl -sf http://127.0.0.1:4000/api/health >/dev/null 2>&1; then
      log "[deploy] hub up (pid=$HUB)"
      exit 0
    fi
  done
  log "[deploy] FAIL: hub did not come back healthy"; exit 1
fi

log "[deploy] done (no restart requested)"
