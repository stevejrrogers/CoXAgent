#!/bin/bash
# CoXAgent host watchdog (CXA-F430) — the agent OUTSIDE the hub.
#
# The hub cannot rescue the hub: every major outage of 2026-10 (launchctl env
# loss ×3 incl. a 17-hour silent pause, a vanished redis desired key, a dead
# Docker daemon) presented as "boot looks healthy, zero cycles, zero CPU" and
# needed a human-or-Claude hand from outside the process. This script IS that
# hand, codified from the live-infra playbook, run by launchd every 5 minutes.
#
# It only ever acts when the hub is provably idle-wedged (health answers but
# the log has been silent too long), and each remedy is the playbook's own:
#   1. restore launchctl envs (incl. the LLM key, read fresh — never stored),
#   2. restore the redis desired-state key,
#   3. restart the app.
# Every action is appended to $WD_LOG and posted to the hub chat once it is
# back, so nothing happens silently.
set -u
HUB_LOG="${COXAGENT_HUB_LOG:-$HOME/CoXAgent/hub.log}"
WD_LOG="${COXAGENT_WATCHDOG_LOG:-$HOME/CoXAgent/watchdog.log}"
APP="${COXAGENT_APP:-$HOME/Projects/CoXAgent/desktop/build/CoXAgent.app}"
PORT="${COXAGENT_PORT:-4000}"
SILENT_SECS="${COXAGENT_WATCHDOG_SILENT_SECS:-2100}"   # 35 min
OPENCODE_JSONC="$HOME/.config/opencode/opencode.jsonc"
COORD="$HOME/CoXAgent/coordination.json"
HOST_ID="root@$(hostname)"

log(){ echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*" >>"$WD_LOG"; }

# 0. Hub reachable at all? A dead process is the app's own crash-restart
#    problem; we only handle the IDLE-WEDGE class (health ok, log silent).
health=$(curl -s -m 5 "http://127.0.0.1:$PORT/api/health" || true)
case "$health" in *'"ok"'*) ;; *) exit 0;; esac

# 1. Log recency — the one reliable liveness signal.
[ -f "$HUB_LOG" ] || exit 0
now=$(date +%s)
mtime=$(stat -f %m "$HUB_LOG" 2>/dev/null || echo "$now")
age=$(( now - mtime ))
[ "$age" -lt "$SILENT_SECS" ] && exit 0

log "WEDGE SUSPECTED: health ok but hub.log silent for ${age}s — running playbook"

# 2. Envs (loss class: 3 incidents in Oct 2026).
op=$(launchctl getenv COXAGENT_OPERATOR || true)
if [ -z "$op" ]; then
  launchctl setenv COXAGENT_OPERATOR root
  launchctl setenv COXAGENT_LLM_BASE_URL https://console.bizbrain.app/v1
  launchctl setenv COXAGENT_MONGO_URL mongodb://127.0.0.1:27017
  KEY=$(/usr/bin/python3 - "$OPENCODE_JSONC" <<'PY'
import re,sys
s=open(sys.argv[1]).read()
m=re.search(r'bizbrain.*?"apiKey"\s*:\s*"([^"]+)"',s,re.S)
print(m.group(1) if m else "")
PY
)
  [ -n "$KEY" ] && launchctl setenv COXAGENT_LLM_API_KEY "$KEY"
  log "restored launchctl envs (OPERATOR was empty)"
fi

# 3. Redis desired key (does not survive docker restarts).
PW=$(/usr/bin/python3 - "$COORD" <<'PY'
import json,re,sys
try:
    u=json.load(open(sys.argv[1]))["redis_url"]
    print(re.search(r"//:(.*?)@",u).group(1))
except Exception:
    print("")
PY
)
if [ -n "$PW" ]; then
  # REDISCLI_AUTH keeps the password out of argv (ps) — never pass -a.
  cur=$(docker exec -e REDISCLI_AUTH="$PW" cxa-backend-redis-1 redis-cli --no-auth-warning GET "cox:cxa:desired:$HOST_ID" 2>/dev/null || true)
  if [ "$cur" != "running" ]; then
    docker exec -e REDISCLI_AUTH="$PW" cxa-backend-redis-1 redis-cli --no-auth-warning SET "cox:cxa:desired:$HOST_ID" running >/dev/null 2>&1 \
      && log "restored redis desired=running (was '${cur:-unreachable}')"
  fi
fi

# 4. Restart the app (quit → force-kill stragglers → open → wait for health).
osascript -e 'quit app "CoXAgent"' >/dev/null 2>&1
n=0; while pgrep -f cox-server >/dev/null && [ $n -lt 10 ]; do sleep 2; n=$((n+1)); done
pkill -9 -f cox-server 2>/dev/null
sleep 2
open "$APP"
n=0; until curl -s -m 3 "http://127.0.0.1:$PORT/api/health" | grep -q ok || [ $n -ge 30 ]; do sleep 3; n=$((n+1)); done
log "restarted hub (health: $(curl -s -m 3 http://127.0.0.1:$PORT/api/health || echo unreachable))"

# 5. Say so where the team will see it (best effort; needs no auth in open
#    mode, degrades silently otherwise — the WD_LOG always has the record).
curl -s -m 5 -X POST "http://127.0.0.1:$PORT/api/chat/send" \
  -H 'Content-Type: application/json' \
  -d "{\"body\":\"🐕 host-watchdog: hub was idle-wedged (log silent ${age}s) — envs/redis checked, app restarted.\"}" >/dev/null 2>&1 || true
exit 0
