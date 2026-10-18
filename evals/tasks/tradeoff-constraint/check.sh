#!/usr/bin/env bash
python3 - <<'PY'
import json,re
raw=open('output.txt').read()
d=json.loads(re.search(r'\{.*\}',raw,re.S).group(0))
assert d['choice']=='B', f"chose {d['choice']}"
assert len(d.get('rejected',{}))>=3
low=json.dumps(d).lower()
assert ('durab' in low or 'crash' in low or 'fsync' in low)
PY
