#!/bin/bash
# CoXAgent host watchdog (CXA-F430) — the agent OUTSIDE the hub.
#
# The hub cannot rescue the hub: every major outage of 2026-10 (launchctl env
# loss ×3 incl. a 17-hour silent pause, a vanished redis desired key, a dead
# Docker daemon) presented as "boot looks healthy, zero cycles, zero CPU" and
# needed a human-or-Claude hand from outside the process. This script IS that
# hand, codified from the live-infra playbook, run by launchd every 5 minutes.
#
# It handles TWO outage classes, each with the playbook's own remedies:
#   A. IDLE-WEDGE — health answers but the log has been silent too long
#      (env loss / redis desired key / stuck cycle): restore envs + redis,
#      then restart the app.
#   B. HARD-DOWN  — the hub PROCESS is gone and health is unreachable. The
#      old script delegated this to "the app's own crash-restart", but a
#      CLEAN app exit has no restarter: on 2026-10-23 the hub exited after a
#      self-heal test run and sat dead 25 min with nobody to bring it back.
#      A missing process is unambiguous, so (after a short debounce that never
#      fights a deploy's own few-second restart) relaunch it.
# Every action is appended to $WD_LOG and posted to the hub chat once it is
# back, so nothing happens silently.
set -u
HUB_LOG="${COXAGENT_HUB_LOG:-$HOME/CoXAgent/hub.log}"
WD_LOG="${COXAGENT_WATCHDOG_LOG:-$HOME/CoXAgent/watchdog.log}"
APP="${COXAGENT_APP:-$HOME/Projects/CoXAgent/desktop/build/CoXAgent.app}"
PORT="${COXAGENT_PORT:-4000}"
SILENT_SECS="${COXAGENT_WATCHDOG_SILENT_SECS:-2100}"   # 35 min
DOWN_DEBOUNCE_SECS="${COXAGENT_WATCHDOG_DOWN_DEBOUNCE_SECS:-15}"
OPENCODE_JSONC="$HOME/.config/opencode/opencode.jsonc"
COORD="$HOME/CoXAgent/coordination.json"
HOST_ID="root@$(hostname)"
REPO="${COXAGENT_REPO:-$HOME/Projects/CoXAgent}"
BUILD_SCRIPT="$REPO/scripts/build-macos-app.sh"
REBUILD_LOCK="$HOME/CoXAgent/.watchdog-rebuild.lock"
REBUILD_STALE_SECS="${COXAGENT_WATCHDOG_REBUILD_STALE_SECS:-2400}"   # 40 min

log(){ echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*" >>"$WD_LOG"; }

# --- Playbook remedies, shared by both outage classes ------------------------

# Restore the launchctl envs (loss class: 3 incidents in Oct 2026). The LLM
# key is read fresh from the opencode config — NEVER stored by this script.
restore_env(){
  local op key
  op=$(launchctl getenv COXAGENT_OPERATOR || true)
  [ -n "$op" ] && return 0
  launchctl setenv COXAGENT_OPERATOR root
  launchctl setenv COXAGENT_LLM_BASE_URL https://console.bizbrain.app/v1
  launchctl setenv COXAGENT_MONGO_URL mongodb://127.0.0.1:27017
  key=$(/usr/bin/python3 - "$OPENCODE_JSONC" <<'PY'
import re,sys
s=open(sys.argv[1]).read()
m=re.search(r'bizbrain.*?"apiKey"\s*:\s*"([^"]+)"',s,re.S)
print(m.group(1) if m else "")
PY
)
  [ -n "$key" ] && launchctl setenv COXAGENT_LLM_API_KEY "$key"
  log "restored launchctl envs (OPERATOR was empty)"
}

# Restore the redis desired-state key (does not survive docker restarts).
restore_redis(){
  local pw cur
  pw=$(/usr/bin/python3 - "$COORD" <<'PY'
import json,re,sys
try:
    u=json.load(open(sys.argv[1]))["redis_url"]
    print(re.search(r"//:(.*?)@",u).group(1))
except Exception:
    print("")
PY
)
  [ -n "$pw" ] || return 0
  # REDISCLI_AUTH keeps the password out of argv (ps) — never pass -a.
  cur=$(docker exec -e REDISCLI_AUTH="$pw" cxa-backend-redis-1 redis-cli --no-auth-warning GET "cox:cxa:desired:$HOST_ID" 2>/dev/null || true)
  [ "$cur" = "running" ] && return 0
  docker exec -e REDISCLI_AUTH="$pw" cxa-backend-redis-1 redis-cli --no-auth-warning SET "cox:cxa:desired:$HOST_ID" running >/dev/null 2>&1 \
    && log "restored redis desired=running (was '${cur:-unreachable}')"
}

# Wait up to ~90s for the hub to answer health after a (re)launch.
wait_health(){
  local n=0
  until curl -s -m 3 "http://127.0.0.1:$PORT/api/health" | grep -q ok || [ $n -ge 30 ]; do
    sleep 3; n=$((n+1))
  done
}

# Tell the team where they will see it (best effort; needs no auth in open
# mode, degrades silently otherwise — the WD_LOG always has the record).
chat_notice(){
  curl -s -m 5 -X POST "http://127.0.0.1:$PORT/api/chat/send" \
    -H 'Content-Type: application/json' \
    -d "{\"body\":\"$1\"}" >/dev/null 2>&1 || true
}

# The app bundle itself can vanish — a cargo clean, a wiped desktop/build, an
# interrupted deploy. When it does, `open "$APP"` is a silent no-op, yet the
# old code logged "relaunched" either way, so the watchdog looped for 30+ min
# claiming success while the hub stayed dead (observed 2026-10-27, bundle gone
# → ~6 false relaunches). Treat a missing bundle as its own outage class: it
# cannot be launched, so (a) never report a relaunch we did not do, and (b)
# self-heal by rebuilding from the repo — but only from a reproducible source
# (clean tree on a named ref, same hygiene deploy-cxa.sh enforces), guarded by
# a lock so 5-minute passes do not stack builds or open a half-assembled app.
# Returns 0 only when the bundle is present AND safe to open right now.
ensure_app_bundle(){
  [ -e "$APP" ] && [ ! -e "$REBUILD_LOCK" ] && return 0
  if [ -e "$REBUILD_LOCK" ]; then
    local lage; lage=$(( $(date +%s) - $(stat -f %m "$REBUILD_LOCK" 2>/dev/null || echo 0) ))
    if [ "$lage" -lt "$REBUILD_STALE_SECS" ]; then
      log "app bundle rebuild already in progress (${lage}s) — deferring relaunch"
      return 1
    fi
    log "stale rebuild lock (${lage}s) — clearing"
    rm -f "$REBUILD_LOCK"
    [ -e "$APP" ] && return 0
  fi
  log "CANNOT RELAUNCH: app bundle $APP is missing"
  if [ ! -x "$BUILD_SCRIPT" ]; then
    log "  no build script at $BUILD_SCRIPT — manual rebuild required"; return 1
  fi
  if ! git -C "$REPO" symbolic-ref --quiet --short HEAD >/dev/null 2>&1 \
     || [ -n "$(git -C "$REPO" status --porcelain 2>/dev/null)" ]; then
    log "  repo not on a clean named ref — refusing auto-rebuild (fix the tree, then it self-heals)"
    return 1
  fi
  : >"$REBUILD_LOCK"
  log "  self-heal: rebuilding bundle from $(git -C "$REPO" rev-parse --short HEAD 2>/dev/null) in background (lock held until done)"
  nohup bash -c "bash '$BUILD_SCRIPT' >>'$WD_LOG.rebuild' 2>&1; rm -f '$REBUILD_LOCK'" >/dev/null 2>&1 &
  return 1
}

# --- Outage detection --------------------------------------------------------

# 0. Hub reachable at all?
health=$(curl -s -m 5 "http://127.0.0.1:$PORT/api/health" || true)
case "$health" in
  *'"ok"'*) ;;   # healthy → fall through to the idle-wedge checks below
  *)
    # CLASS B — HARD-DOWN. Debounce briefly so a deploy's own few-second
    # restart (process present again within ~1s) is never fought, then act
    # only when the PROCESS is genuinely absent. A process that exists but is
    # not yet serving is mid-boot or unhealthy — leave it for the next pass
    # rather than killing a booting hub.
    sleep "$DOWN_DEBOUNCE_SECS"
    h2=$(curl -s -m 5 "http://127.0.0.1:$PORT/api/health" || true)
    case "$h2" in *'"ok"'*) exit 0;; esac
    if pgrep -f 'cox-server hub' >/dev/null 2>&1; then
      exit 0
    fi
    log "HARD-DOWN: cox-server process absent + health unreachable (debounced ${DOWN_DEBOUNCE_SECS}s) — restoring env/redis and relaunching"
    restore_env
    restore_redis
    if ! ensure_app_bundle; then
      log "hard-down unresolved this pass (bundle unavailable) — will retry next pass"
      exit 0
    fi
    # Process is confirmed gone, so no quit/pkill — just launch it.
    open "$APP"
    wait_health
    h=$(curl -s -m 3 "http://127.0.0.1:$PORT/api/health" || echo unreachable)
    log "relaunched after hard-down (health: $h)"
    case "$h" in
      *'"ok"'*) chat_notice "🐕 host-watchdog: hub process was gone (hard-down) — envs/redis checked, app relaunched." ;;
      *) log "relaunch did NOT restore health — bundle present but hub not serving; escalating for next pass" ;;
    esac
    exit 0;;
esac

# 1. Log recency — the one reliable liveness signal for the IDLE-WEDGE class.
[ -f "$HUB_LOG" ] || exit 0
now=$(date +%s)
mtime=$(stat -f %m "$HUB_LOG" 2>/dev/null || echo "$now")
age=$(( now - mtime ))
[ "$age" -lt "$SILENT_SECS" ] && exit 0

log "WEDGE SUSPECTED: health ok but hub.log silent for ${age}s — running playbook"

# CLASS A — IDLE-WEDGE: restore envs + redis, then full restart.
restore_env
restore_redis

# Confirm we can actually relaunch BEFORE we kill the (wedged but serving) hub.
# A wedged hub that still answers health beats a dead one we cannot bring back.
if ! ensure_app_bundle; then
  log "idle-wedge: bundle unavailable — leaving the current hub untouched, will retry next pass"
  exit 0
fi

# Restart the app (quit → force-kill stragglers → open → wait for health).
osascript -e 'quit app "CoXAgent"' >/dev/null 2>&1
n=0; while pgrep -f cox-server >/dev/null && [ $n -lt 10 ]; do sleep 2; n=$((n+1)); done
pkill -9 -f cox-server 2>/dev/null
sleep 2
open "$APP"
wait_health
log "restarted hub (health: $(curl -s -m 3 http://127.0.0.1:$PORT/api/health || echo unreachable))"
chat_notice "🐕 host-watchdog: hub was idle-wedged (log silent ${age}s) — envs/redis checked, app restarted."
exit 0
