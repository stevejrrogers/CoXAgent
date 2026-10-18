#!/usr/bin/env bash
# watchdog-cxa.sh — Overnight watchdog for the CoXAgent live stack.
# Monitors: hub health, scorecard regression (rejected 'Debt sweep' noise),
# and logs actionable alerts. Light self-heal only; defers code-fix/deploy
# decisions to the agent by writing clear alerts to the log.
#
# Run detached:
#   nohup bash deploy/watchdog-cxa.sh >> deploy/watchdog.log 2>&1 &
# or single check:
#   bash deploy/watchdog-cxa.sh --oneshot
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HUB_HEALTH="http://127.0.0.1:4000/api/health"
LOG="$ROOT/deploy/watchdog.log"
COOKIE_FILE="$ROOT/deploy/.watchdog-cookie"
ONESHOT="${1:-}"

log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*" | tee -a "$LOG"; }

# Login once and cache the session cookie (never log the password).
ensure_cookie() {
  local pw tok
  pw=$(cat /Users/luton/CoXAgent/admin-password 2>/dev/null || true)
  [ -n "$pw" ] || { log "ERROR: no admin password"; return 1; }
  tok=$(curl -sS -m 10 -i -X POST http://127.0.0.1:4000/api/auth/login \
        -H 'Content-Type: application/json' \
        -d "{\"username\":\"root\",\"password\":\"$pw\"}" 2>/dev/null \
        | grep -i '^set-cookie:' | sed -n 's/set-cookie: cox_session=\([^;]*\).*/\1/p')
  if [ -n "$tok" ]; then
    printf '%s\n' "$tok" > "$COOKIE_FILE"
    return 0
  fi
  return 1
}

hub_alive() {
  curl -sS -m 5 "$HUB_HEALTH" 2>/dev/null | grep -q '"ok"'
}

opencode_workers() {
  pgrep -f opencode 2>/dev/null | wc -l | tr -d ' '
}

# Prints number of rejected 'Debt sweep' tickets (>0 => regression signal).
rejected_sweep_count() {
  local tok
  tok=$(cat "$COOKIE_FILE" 2>/dev/null || true)
  [ -n "$tok" ] || { echo -1; return; }
  python3 -c "
import json,urllib.request
tok='$tok'
req=urllib.request.Request('http://127.0.0.1:4000/api/projects/cxa/store?op=load',data=b'{}',
  headers={'Content-Type':'application/json','Cookie':'cox_session='+tok},method='POST')
try:
    j=json.load(urllib.request.urlopen(req,timeout=8))
    ts=j.get('tickets',[])
    print(sum(1 for t in ts if t.get('status')=='rejected' and 'Debt sweep' in (t.get('title') or '')))
except Exception:
    print(-1)
"
}

# Restart the hub by TERM-ing its process; the desktop app shell auto-restarts it.
# Waits up to ~30s for a new hub PID to appear and pass health.
restart_hub() {
  local old new i hv
  old=$(pgrep -f 'cox-server hub' | head -1 || true)
  log "HEAL: restarting hub (old_pid=${old:-none})"
  if [ -n "$old" ]; then
    kill -TERM "$old" 2>/dev/null || true
  fi
  for i in $(seq 1 15); do
    sleep 2
    new=$(pgrep -f 'cox-server hub' | grep -v "^${old}$" | head -1 || true)
    if [ -n "$new" ] && hub_alive; then
      hv=$(pgrep -f 'cox-server hub' | head -1)
      log "HEAL-DONE: hub up (pid=${hv})"
      # clear any leftover cookie (hub restart may drop sessions)
      rm -f "$COOKIE_FILE"; ensure_cookie || true
      return 0
    fi
  done
  log "HEAL-FAILED: hub did not come back healthy after restart"
  return 1
}

# Atomic cleanup of rejected 'Debt sweep' noise tickets via the store RPC
# (load -> filter -> save with revision conflict retry). Returns 0 if nothing
# to do or cleaned; 1 on auth/API failure.
clean_sweep() {
  local tok out
  tok=$(cat "$COOKIE_FILE" 2>/dev/null || true)
  [ -n "$tok" ] || { log "HEAL: no cookie, skipping sweep cleanup"; return 1; }
  out=$(python3 - "$tok" <<'PY'
import json,sys,time,urllib.request,urllib.error
tok=sys.argv[1]
BASE='http://127.0.0.1:4000/api/projects/cxa/store'
def post(op,payload):
    req=urllib.request.Request(f'{BASE}?op={op}',data=json.dumps(payload).encode(),
      headers={'Content-Type':'application/json','Cookie':'cox_session='+tok},method='POST')
    try:
        with urllib.request.urlopen(req,timeout=10) as r: return r.status,json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        try: return e.code,json.loads(e.read().decode())
        except Exception: return e.code,{}
for _ in range(8):
    _,v=post('version',{}); rev=v.get('revision')
    _,st=post('load',{}); ts=st.get('tickets',[])
    keep=[t for t in ts if not (t.get('status')=='rejected' and 'Debt sweep' in (t.get('title') or ''))]
    if len(keep)==len(ts):
        print('none'); sys.exit(0)
    st['tickets']=keep
    code,j=post('save',{'revision':rev,'data':json.dumps(st)})
    if code==200 and j.get('ok'):
        print(f'removed={len(ts)-len(keep)} rev={rev}'); sys.exit(0)
    if code==409:
        time.sleep(1); continue
    print(f'error={code}'); sys.exit(1)
print('conflict-retries-exhausted'); sys.exit(1)
PY
)
  local rc=$?
  log "HEAL-SWEEP: $out (rc=$rc)"
  return $rc
}

check_once() {
  if ! hub_alive; then
    log "ALERT: HUB DOWN (workers=$(opencode_workers)) — attempting self-heal"
    restart_hub
    if ! hub_alive; then
      return
    fi
  fi
  local ws c
  ws=$(opencode_workers)
  c=$(rejected_sweep_count)
  if [ "$c" -ge 1 ]; then
    log "REGRESSION: ${c} rejected 'Debt sweep' ticket(s) present — auto-cleaning"
    clean_sweep
    # re-read after cleanup
    c=$(rejected_sweep_count)
    if [ "$c" -ge 1 ]; then
      log "REGRESSION-PERSIST: ${c} rejected 'Debt sweep' still present after clean — likely code path re-filing; needs agent"
    fi
  fi
  log "OK hub=alive workers=${ws} rejected_debt_sweep=${c}"
}

if [ "$ONESHOT" = "--oneshot" ]; then
  ensure_cookie || exit 1
  check_once
  exit 0
fi

ensure_cookie || true
log "watchdog-cxa started (pid $$), root=$ROOT"

while true; do
  check_once
  # refresh cookie roughly every 10 minutes to avoid expiry staleness
  if [ $((SECONDS % 600)) -lt 5 ]; then
    ensure_cookie || true
  fi
  sleep 60
done
