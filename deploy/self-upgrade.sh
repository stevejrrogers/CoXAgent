#!/usr/bin/env bash
# self-upgrade.sh — the hub upgrades ITSELF from origin/<base>, with rollback.
#
# Runs as a detached process so a dying hub cannot orphan its own rescue:
# fetch → new commit? → build → swap (backup kept) → codesign → restart →
# health-check → rollback to the backup binary if the new hub never comes up.
#
# Args: REPO_DIR TARGET_BINARY PORT [BASE]
#   REPO_DIR       git clone to build from (e.g. ~/CoXAgent/cox/codebase)
#   TARGET_BINARY  the binary the app shell launches (…/CoXAgent.app/Contents/MacOS/cox-server)
#   PORT           hub port to health-check (e.g. 4000)
#   BASE           branch to track (default main)
#
# State: <REPO_DIR>/.coxagent-self-upgrade/ holds the last-deployed sha, a
# lockfile, and the log. Exits 0 quietly when there is nothing to do.
set -u

REPO_DIR="${1:?repo dir}"
TARGET="${2:?target binary}"
PORT="${3:?port}"
BASE="${4:-main}"

STATE_DIR="$REPO_DIR/.coxagent-self-upgrade"
mkdir -p "$STATE_DIR"
LOG="$STATE_DIR/upgrade.log"
LOCK="$STATE_DIR/lock"
SHA_FILE="$STATE_DIR/deployed-sha"

log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*" >> "$LOG"; }

# One upgrade at a time. Staleness is judged by whether the OWNING PROCESS is
# alive, not by age: a cold build on a loaded machine can exceed any timer, and
# an age-based takeover once put two instances into the same build worktree —
# they destroyed each other's build and both "failed" in the same second.
if [ -d "$LOCK" ]; then
  owner=$(cat "$LOCK/pid" 2>/dev/null || echo "")
  if [ -n "$owner" ] && kill -0 "$owner" 2>/dev/null; then
    exit 0 # a live run owns the lock — never take it over
  fi
  rm -rf "$LOCK"
fi
mkdir "$LOCK" || exit 0
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

cd "$REPO_DIR" || exit 1
git fetch -q origin "$BASE" 2>>"$LOG" || { log "fetch failed"; exit 1; }
NEW_SHA=$(git rev-parse "origin/$BASE" 2>/dev/null) || exit 1
OLD_SHA=$(cat "$SHA_FILE" 2>/dev/null || echo "")
[ "$NEW_SHA" = "$OLD_SHA" ] && exit 0

log "upgrade candidate: ${OLD_SHA:-none} -> $NEW_SHA"

# Build from a detached persistent worktree so the working tree (agents may be
# mid-edit in it) is never touched. The worktree is KEPT between runs: its
# target/ makes every upgrade after the first an incremental build (minutes,
# not an hour on a loaded machine).
BUILD_WT="$STATE_DIR/build-tree"
if [ -d "$BUILD_WT/.git" ] || [ -f "$BUILD_WT/.git" ]; then
  git -C "$BUILD_WT" checkout --detach -f "$NEW_SHA" >>"$LOG" 2>&1 \
    || { log "worktree checkout failed"; exit 1; }
else
  git worktree remove --force "$BUILD_WT" >/dev/null 2>&1
  git worktree add --detach "$BUILD_WT" "$NEW_SHA" >>"$LOG" 2>&1 \
    || { log "worktree add failed"; exit 1; }
fi
if ! (cd "$BUILD_WT" && cargo build --release --bin coxagent >>"$LOG" 2>&1); then
  log "BUILD FAILED for $NEW_SHA — keeping current hub"
  exit 1
fi
NEW_BIN="$BUILD_WT/target/release/coxagent"
"$NEW_BIN" --version >>"$LOG" 2>&1 || { log "new binary does not run"; exit 1; }

# Swap with a backup; the previous binary is the rollback.
cp -X "$TARGET" "$TARGET.prev" 2>>"$LOG"
cp -X "$NEW_BIN" "$TARGET" || { log "copy failed"; exit 1; }
codesign --force --sign - "$TARGET" >>"$LOG" 2>&1

# Restart: kill the listener; the app shell respawns the binary.
PID=$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | awk 'NR==2{print $2}')
[ -n "$PID" ] && kill "$PID" 2>/dev/null

# Health: the new hub must answer within 60s, else roll back and restart again.
ok=""
for _ in $(seq 1 60); do
  sleep 1
  NP=$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | awk 'NR==2{print $2}')
  if [ -n "$NP" ] && [ "$NP" != "${PID:-}" ]; then
    curl -sf -m 3 "http://localhost:$PORT/" >/dev/null 2>&1 && { ok=1; break; }
  fi
done

if [ -n "$ok" ]; then
  echo "$NEW_SHA" > "$SHA_FILE"
  log "UPGRADED to $NEW_SHA (hub answering on :$PORT)"
else
  log "HEALTH CHECK FAILED — rolling back to previous binary"
  cp -X "$TARGET.prev" "$TARGET" 2>>"$LOG"
  codesign --force --sign - "$TARGET" >>"$LOG" 2>&1
  NP=$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | awk 'NR==2{print $2}')
  [ -n "$NP" ] && kill "$NP" 2>/dev/null
  log "rollback issued; shell will respawn the previous hub"
fi
