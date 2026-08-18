#!/usr/bin/env python3
"""Watchdog loop #2: focus on CXA-B051 (/api/openapi.json 404 fix).

Watches for:
 1. A new PR/branch for CXA-B051; report head branch and whether the diff is
    CLEAN (only openapi/presentation files) vs MESSY (mergetest/, wiki/,
    config_drift, docker_compose, unrelated changes).
 2. Whether /api/openapi.json on localhost:4000 changes from 404.
 3. New build/deploy to the live hub (upgrade.log new MANUAL UPGRADE /
    UPGRADED lines, version bump).
 4. Any new anomaly (branch failure, stall > 8min, new bug dup).

Observes and logs only. No code edits, builds, deploys, merges, pushes, comments.
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
# Messy-diff markers for a B051 PR
MESSY_MARKERS = ['mergetest/', 'wiki/', 'config_drift', 'docker_compose', 'docker-compose']

def now_utc_iso():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def append_alert(msg):
    with open(ALERTS_LOG, 'a') as f:
        f.write(f"[{now_utc_iso()}] {msg}\n")

def fetch_activity():
    conn = psycopg2.connect(**PG)
    cur = conn.cursor()
    cur.execute("SELECT data::text FROM project_state WHERE project_id='cxa'")
    d = json.loads(cur.fetchone()[0])
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

def check_endpoint():
    try:
        r = subprocess.run(['curl', '-s', '-o', '/dev/null', '-w', '%{http_code}',
                            '--max-time', '10', 'http://localhost:4000/api/openapi.json'],
                           capture_output=True, text=True, timeout=15)
        return r.stdout.strip()
    except Exception as e:
        return 'ERR'

# Analyze a PR's diff to classify CLEAN vs MESSY and to find B051-related files.
def analyze_pr_diff(number):
    try:
        r = subprocess.run(['gh', 'pr', 'diff', str(number)],
                           capture_output=True, text=True, timeout=60)
        if r.returncode != 0:
            return {'error': r.stderr.strip()[:200]}
        diff = r.stdout
        # Collect filenames touched
        files = re.findall(r'^\+\+\+ b/(.+)$', diff, re.M)
        files = [f for f in files if f != '/dev/null']
        # Extension set
        exts = set(os.path.splitext(f)[1].lower() for f in files)
        messy_hits = [m for m in MESSY_MARKERS if any(m in f for f in files)]
        openapi_hits = [f for f in files if 'openapi' in f.lower()]
        web_hits = [f for f in files if f.startswith(('web/', 'presentation/'))]
        branch_added = ('+branch:' in diff) or re.search(r'^[+-].*branch', diff, re.M)
        verdict = 'MESSY' if messy_hits else 'CLEAN'
        return {
            'files': files,
            'n_files': len(files),
            'exts': sorted(exts),
            'messy_hits': messy_hits,
            'openapi_files': openapi_hits,
            'web_presentation': web_hits,
            'verdict': verdict,
            'len': len(diff),
        }
    except Exception as e:
        return {'error': str(e)[:200]}

def main():
    last_log = {}
    def throttle(key, seconds=300):
        now = time.time()
        if key in last_log and now - last_log[key] < seconds:
            return False
        last_log[key] = now
        return True

    # baseline
    baseline_titles = set()
    baseline_initialized = False
    baseline_bug_count = None
    endpoint_baseline = check_endpoint()
    append_alert(f'B051-MON START baseline endpoint /api/openapi.json = {endpoint_baseline}')

    iterations = 0
    start_wall = time.time()
    DURATION = 900

    while time.time() - start_wall < DURATION:
        iterations += 1
        it = iterations
        time.sleep(60)

        # heartbeat
        hb_txt = ''
        try:
            with open(WATCH_OUT, 'r') as f:
                hb_txt = f.read().strip()
        except Exception:
            pass

        # activity + worker
        act, tickets = [], []
        try:
            act, tickets = fetch_activity()
        except Exception as e:
            if throttle('pg_err'):
                append_alert(f'ITER {it}: Postgres error {e}')
        try:
            worker = fetch_worker()
        except Exception:
            worker = None

        newest_at = act[-1].get('at') if act else None
        newest_action = act[-1].get('action') if act else None

        # open PRs
        prs = []
        try:
            r = subprocess.run(['gh', 'pr', 'list', '--state', 'open',
                                '--json', 'number,title,headRefName'],
                               capture_output=True, text=True, timeout=30)
            if r.returncode == 0:
                prs = json.loads(r.stdout)
        except Exception:
            pass

        # --- anomaly: stall ---
        if newest_at:
            n = parse_iso(newest_at)
            if n:
                mins = (datetime.datetime.now(datetime.timezone.utc) - n).total_seconds() / 60.0
                if mins > 8 and throttle('stall'):
                    append_alert(f'ITER {it} STALL: {mins:.1f} min (newest={newest_at}) {newest_action}')

        # --- anomaly: branch failure ---
        for a in act[-15:]:
            t = a.get('action') or ''
            if re.search(r'branch\s+.*\s+failed', t, re.IGNORECASE) and throttle('branch_fail'):
                append_alert(f'ITER {it} BRANCH-FAIL: {a.get("at")} {t}')

        # --- anomaly: new bug dup ---
        if not baseline_initialized:
            for t in tickets:
                if (t.get('type') or '').lower() == 'bug':
                    baseline_titles.add(t.get('id'))
            baseline_bug_count = len(baseline_titles)
            baseline_initialized = True
        else:
            for t in tickets:
                if (t.get('type') or '').lower() == 'bug' and t.get('id') not in baseline_titles:
                    baseline_titles.add(t.get('id'))
                    m = KEYWORDS_RE.search(t.get('title') or '')
                    if m and throttle('dup_' + m.group(0).lower()):
                        append_alert(f'ITER {it} BUG-DUP: {t.get("id")} "{t.get("title")}" matches "{m.group(0)}"')

        # --- endpoint 404 status ---
        code = check_endpoint()
        if code != '404' and throttle('endpoint_changed'):
            append_alert(f'ITER {it} ENDPOINT-CHANGED: /api/openapi.json now returns {code} (was 404)')
        if code != '404':
            print(f'[iter {it}] *** openapi endpoint now {code} ***')

        # --- B051 PR detection ---
        for p in prs:
            head = p.get('headRefName') or ''
            title = p.get('title') or ''
            num = p.get('number')
            if 'B051' in head.upper() or 'B051' in title.upper() or 'openapi' in title.lower():
                if throttle(f'b051pr_{num}'):
                    analysis = analyze_pr_diff(num)
                    append_alert(
                        f'ITER {it} B051-PR: #{num} head={head} title="{title}" '
                        f'verdict={analysis.get("verdict")} n_files={analysis.get("n_files")} '
                        f'files={json.dumps(analysis.get("files"))} '
                        f'messy_hits={json.dumps(analysis.get("messy_hits"))}')

        # --- upgrade log / deployed version ---
        try:
            with open(UPGRADE_LOG, 'r') as f:
                ul = f.read()
            for line in ul.splitlines():
                if ('MANUAL UPGRADE' in line or 'UPGRADED to' in line) and throttle('upgrade_' + line[:40]):
                    append_alert(f'ITER {it} UPGRADE: {line}')
            if 'BUILD FAILED' in ul or 'HEALTH CHECK FAILED' in ul:
                if throttle('build_fail'):
                    append_alert(f'ITER {it} BUILD/DEPLOY-FAIL in {UPGRADE_LOG}')
        except Exception:
            pass

        # hub health
        try:
            r = subprocess.run(['curl', '-s', '-o', '/dev/null', '-w', '%{http_code}',
                                '--max-time', '10', 'http://localhost:4000/'],
                               capture_output=True, text=True, timeout=15)
            hub_ok = r.stdout.strip() == '200'
        except Exception:
            hub_ok = False
        if not hub_ok and throttle('hub_down'):
            append_alert(f'ITER {it} HUB-DOWN: :4000 not 200 (logged only)')

        tail = hb_txt.strip().splitlines()[-1] if hb_txt.strip().splitlines() else 'none'
        print(f"[{now_utc_iso()}] iter={it} worker={(worker or {}).get('ticket')} "
              f"newest={newest_at} openapi={code} open_prs={[p['number'] for p in prs]} hub={'UP' if hub_ok else 'DOWN'} tail={tail[:120]}")

    # final summary
    append_alert('===== B051-WATCH FINAL SUMMARY =====')
    try:
        act_f, tickets_f = fetch_activity()
        worker_f = fetch_worker()
        append_alert(f'worker: {json.dumps(worker_f)}')
        append_alert(f'newest activity: {act_f[-1].get("at")} | {act_f[-1].get("action")}')
    except Exception as e:
        append_alert(f'snapshot error {e}')
    code_f = check_endpoint()
    append_alert(f'openapi endpoint final: {code_f}')
    try:
        r = subprocess.run(['gh', 'pr', 'list', '--state', 'open', '--json', 'number,title,headRefName'],
                           capture_output=True, text=True, timeout=30)
        prs_f = json.loads(r.stdout) if r.returncode == 0 else []
        append_alert('open PRs: ' + json.dumps(prs_f))
    except Exception:
        pass
    try:
        with open(UPGRADE_LOG, 'r') as f:
            append_alert('upgrade.log tail:\n' + '\n'.join(f.read().splitlines()[-6:]))
    except Exception:
        pass
    try:
        r = subprocess.run(['curl', '-s', '-o', '/dev/null', '-w', '%{http_code}', '--max-time', '10', 'http://localhost:4000/'],
                           capture_output=True, text=True, timeout=15)
        append_alert(f'hub :4000 = {"UP (200)" if r.stdout.strip()=="200" else r.stdout.strip()}')
    except Exception:
        pass
    append_alert(f'iterations: {iterations}')
    append_alert('===== END SUMMARY =====')

if __name__ == '__main__':
    main()
