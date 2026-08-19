#!/usr/bin/env python3
"""Continuous watchdog loop for CoXAgent.

Observes live state, logs anomalies to /tmp/cox_alerts.log (deduped per 5 min),
never edits/builds/deploys/merges. Only logs + optional safe PR comments.

Anomalies monitored:
  - Stall: no new activity in project_state activity for > 8 min vs current UTC.
  - PR close without merge: activity action matching 'close' (just log text).
  - Branch failure spam: activity action containing 'branch ... failed'.
  - Build/deploy failure: upgrade.log contains 'BUILD FAILED'/'HEALTH CHECK FAILED'.
  - New bug-ticket duplicate: new bug titles matching known root-cause keywords.
"""
import json, os, subprocess, time, datetime, re
import psycopg2
import redis

PG = dict(host='127.0.0.1', port=5433, user='coxagent',
          password='Wks8skU0OuCmbmid1CHHfhCpKxnHF9yu', dbname='coxagent')
REDIS_URL = 'redis://:HQz6hXCfw6dAJ3nQF5cQQTaan3HnZeZi@127.0.0.1:6379/0'
WORKER_KEY = 'cox:cxa:worker:root@Lutons-MacBook-Pro.local'
ALERTS_LOG = '/tmp/cox_alerts.log'
WATCH_OUT = '/tmp/cox_watch2.out'
UPGRADE_LOG = '/Users/luton/Projects/CoXAgent/.coxagent-self-upgrade/upgrade.log'

ROOT_CAUSE_KEYWORDS = [
    'docker compose failed', 'interpolating', 'pg_password', 'cargo tests failed',
    'tests failing', 'no response on port', 'openapi', 're-export guard',
    'secret rotation', 'world-readable',
]

KEYWORDS_RE = re.compile('|'.join(re.escape(k) for k in ROOT_CAUSE_KEYWORDS), re.IGNORECASE)

def now_utc_iso():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def append_alert(msg):
    with open(ALERTS_LOG, 'a') as f:
        f.write(f"[{now_utc_iso()}] {msg}\n")

def fetch_activity():
    conn = psycopg2.connect(**PG)
    cur = conn.cursor()
    cur.execute("SELECT data::text FROM project_state WHERE project_id='cxa'")
    row = cur.fetchone()
    d = json.loads(row[0])
    act = d.get('activity', [])
    tickets = d.get('tickets', [])
    conn.close()
    return act, tickets

def fetch_worker():
    r = redis.Redis.from_url(REDIS_URL)
    raw = r.get(WORKER_KEY)
    if not raw:
        return None
    wj = json.loads(raw)
    return {'role': wj.get('role'), 'ticket': wj.get('ticket'), 'at': wj.get('at'), 'worker': wj.get('worker')}

def parse_iso(s):
    try:
        return datetime.datetime.fromisoformat(s.replace('Z', '+00:00'))
    except Exception:
        return None

def main():
    # Track last-logged times per anomaly type for 5-min dedup, and baseline bug titles.
    last_log = {}
    baseline_titles = set()
    baseline_initialized = False

    def throttle(key, seconds=300):
        now = time.time()
        if key in last_log and now - last_log[key] < seconds:
            return False
        last_log[key] = now
        return True

    iterations = 0
    start_wall = time.time()
    DURATION = 900  # seconds (~15 min)

    while time.time() - start_wall < DURATION:
        iterations += 1
        it = iterations
        time.sleep(60)

        # --- 1. heartbeat ---
        hb_txt = ''
        try:
            with open(WATCH_OUT, 'r') as f:
                lines = f.read().strip().splitlines()
                hb_txt = '\n'.join(lines[-5:])
        except Exception as e:
            hb_txt = f'<cannot read {WATCH_OUT}: {e}>'

        # --- 2/3. postgres + redis ---
        act, tickets = [], []
        try:
            act, tickets = fetch_activity()
        except Exception as e:
            if throttle(f'pg_err_{it}'):
                append_alert(f'ITER {it}: Postgres query error: {e}')

        try:
            worker = fetch_worker()
        except Exception as e:
            worker = None
            if throttle(f'redis_err_{it}'):
                append_alert(f'ITER {it}: Redis query error: {e}')

        # newest activity
        newest_at = None
        newest_action = None
        if act:
            newest_at = act[-1].get('at')
            newest_action = act[-1].get('action')

        # --- 4. open PRs ---
        prs = []
        try:
            r = subprocess.run(['gh', 'pr', 'list', '--state', 'open',
                                '--json', 'number,title,headRefName'],
                               capture_output=True, text=True, timeout=30)
            if r.returncode == 0:
                prs = json.loads(r.stdout)
        except Exception as e:
            if throttle(f'gh_err_{it}'):
                append_alert(f'ITER {it}: gh pr list error: {e}')

        # --- build/deploy failure grep ---
        build_fail = False
        try:
            if os.path.exists(UPGRADE_LOG):
                with open(UPGRADE_LOG, 'r') as f:
                    ul = f.read()
                if 'BUILD FAILED' in ul or 'HEALTH CHECK FAILED' in ul:
                    build_fail = True
        except Exception:
            pass

        # --- ANOMALY: stall (>8 min no new activity) ---
        if newest_at:
            n = parse_iso(newest_at)
            if n:
                minutes_since = (datetime.datetime.now(datetime.timezone.utc) - n).total_seconds() / 60.0
                if minutes_since > 8:
                    if throttle(f'stall'):
                        append_alert(
                            f'ITER {it} STALL: no new activity for {minutes_since:.1f} min '
                            f'(newest_at={newest_at}). Last activity: {newest_action}')

        # --- ANOMALY: PR close without merge (log action text only) ---
        # Scan recent activity for 'close' actions
        for a in act[-10:]:
            act_text = (a.get('action') or '')
            if re.search(r'\bclose', act_text, re.IGNORECASE):
                if throttle('pr_close'):
                    append_alert(f'ITER {it} PR-CLOSE: {a.get("at")} {a.get("agent")}: {act_text}')

        # --- ANOMALY: branch failure spam ---
        for a in act[-10:]:
            act_text = (a.get('action') or '')
            if re.search(r'branch\s+.*\s+failed', act_text, re.IGNORECASE):
                if throttle('branch_fail'):
                    append_alert(f'ITER {it} BRANCH-FAIL: {a.get("at")} {a.get("agent")}: {act_text}')

        # --- ANOMALY: build/deploy failure ---
        if build_fail:
            if throttle('build_fail'):
                append_alert(f'ITER {it} BUILD/DEPLOY-FAIL: {UPGRADE_LOG} shows BUILD FAILED or HEALTH CHECK FAILED')

        # --- ANOMALY: new bug-ticket duplicate ---
        # First iteration: record baseline bug ticket ids+keywords.
        if not baseline_initialized:
            for t in tickets:
                if (t.get('type') or '').lower() == 'bug':
                    baseline_titles.add(t.get('id'))
            baseline_initialized = True
        else:
            for t in tickets:
                ttype = (t.get('type') or '').lower()
                title = (t.get('title') or '')
                tid = t.get('id')
                if ttype == 'bug' and tid not in baseline_titles:
                    baseline_titles.add(tid)
                    m = KEYWORDS_RE.search(title)
                    if m:
                        kw = m.group(0).lower()
                        if throttle('dup_' + kw):
                            append_alert(
                                f'ITER {it} BUG-DUP: new bug ticket {tid} "{title}" '
                                f'matches root-cause keyword "{kw}"')

        # --- hub health ---
        hub_ok = False
        try:
            r = subprocess.run(['curl', '-s', '-o', '/dev/null', '-w', '%{http_code}',
                                '--max-time', '10', 'http://localhost:4000/'],
                               capture_output=True, text=True, timeout=15)
            hub_ok = (r.stdout.strip() == '200')
        except Exception:
            hub_ok = False
        if not hub_ok and throttle('hub_down'):
            append_alert(f'ITER {it} HUB-DOWN: cox-server not answering 200 on :4000 (logged; orchestrator handles restart)')

        print(f"[{now_utc_iso()}] iter={it} newest_at={newest_at} worker_ticket={(worker or {}).get('ticket')} "
              f"hub={'UP' if hub_ok else 'DOWN'} open_prs={len(prs)} hb_tail={hb_txt.splitlines()[-1] if hb_txt and hb_txt.splitlines() else 'none'}")

    # --- final summary ---
    append_alert('===== WATCHDOG FINAL SUMMARY =====')
    try:
        act_final, tickets_final = fetch_activity()
        worker_final = fetch_worker()
        append_alert(f'Final worker state: {json.dumps(worker_final)}')
        append_alert(f'Final newest activity: {act_final[-1].get("at") if act_final else None}: {act_final[-1].get("action") if act_final else None}')
    except Exception as e:
        append_alert(f'Final snapshot error: {e}')
    try:
        r = subprocess.run(['gh', 'pr', 'list', '--state', 'open', '--json', 'number,title,headRefName'],
                           capture_output=True, text=True, timeout=30)
        prs_final = json.loads(r.stdout) if r.returncode == 0 else []
        append_alert(f'Open PRs: {json.dumps(prs_final)}')
    except Exception:
        append_alert('Open PRs: <gh error>')
    try:
        r = subprocess.run(['curl', '-s', '-o', '/dev/null', '-w', '%{http_code}', '--max-time', '10', 'http://localhost:4000/'],
                           capture_output=True, text=True, timeout=15)
        append_alert(f'Hub health on :4000: {"UP (200)" if r.stdout.strip()=="200" else "DOWN " + r.stdout.strip()}')
    except Exception:
        append_alert('Hub health: <curl error>')
    append_alert(f'Watchdog iterated {iterations} times over ~{DURATION//60} min')
    append_alert('===== END SUMMARY =====')

if __name__ == '__main__':
    main()
