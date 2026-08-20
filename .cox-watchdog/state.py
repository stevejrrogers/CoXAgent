#!/usr/bin/env python3
"""Watchdog helper: dump live agent state (Postgres activity + Redis worker)."""
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
        out['activity'] = act[-10:]  # last 10
        out['newest_at'] = act[-1].get('at') if act else None
        out['n_activity'] = len(act)
        out['tickets'] = d.get('tickets', [])
        out['swept_tickets'] = d.get('swept_tickets')
        conn.close()
    except Exception as e:
        out['pg_error'] = str(e)

    try:
        r = redis.Redis.from_url(REDIS_URL)
        out['redis_worker'] = r.hgetall(WORKER_KEY)
    except Exception as e:
        out['redis_error'] = str(e)

    print(json.dumps(out))

if __name__ == '__main__':
    main()
