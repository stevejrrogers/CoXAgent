#!/usr/bin/env python3
"""Watchdog helper v2: compact live state for monitoring (no huge dumps)."""
import json
import psycopg2
import redis

PG = dict(host='127.0.0.1', port=5433, user='coxagent',
          password='Wks8skU0OuCmbmid1CHHfhCpKxnHF9yu', dbname='coxagent')
REDIS_URL = 'redis://:HQz6hXCfw6dAJ3nQF5cQQTaan3HnZeZi@127.0.0.1:6379/0'
WORKER_KEY = 'cox:cxa:worker:root@Lutons-MacBook-Pro.local'

def main():
    out = {}
    try:
        conn = psycopg2.connect(**PG)
        cur = conn.cursor()
        cur.execute("SELECT data::text FROM project_state WHERE project_id='cxa'")
        row = cur.fetchone()
        d = json.loads(row[0])
        act = d.get('activity', [])
        out['newest_at'] = act[-1].get('at') if act else None
        out['newest_action'] = act[-1].get('action') if act else None
        out['n_activity'] = len(act)
        # last 3 actions
        out['last3'] = [(a.get('at'), a.get('agent'), a.get('action')) for a in act[-3:]]
        # tickets: count by status and the bug tickets
        tix = d.get('tickets', [])
        out['n_tickets'] = len(tix)
        from collections import Counter
        out['status_counts'] = dict(Counter(t.get('status') for t in tix))
        # recent tickets (titles)
        out['ticket_titles'] = [(t.get('id'), t.get('title'), t.get('type'), t.get('status')) for t in tix[-15:]]
        conn.close()
    except Exception as e:
        out['pg_error'] = str(e)

    try:
        r = redis.Redis.from_url(REDIS_URL)
        wt = r.type(WORKER_KEY)
        if wt == b'hash':
            w = r.hgetall(WORKER_KEY)
            out['redis_worker'] = {k.decode() if isinstance(k, bytes) else k:
                                   v.decode() if isinstance(v, bytes) else v for k, v in w.items()}
        else:
            raw = r.get(WORKER_KEY)
            if raw:
                wj = json.loads(raw)
                out['redis_worker'] = {
                    'role': wj.get('role'),
                    'ticket': wj.get('ticket'),
                    'at': wj.get('at'),
                    'worker': wj.get('worker'),
                }
            else:
                out['redis_worker'] = None
    except Exception as e:
        out['redis_error'] = str(e)

    print(json.dumps(out, default=str))

if __name__ == '__main__':
    main()
