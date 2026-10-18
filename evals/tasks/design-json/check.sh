#!/usr/bin/env bash
python3 - <<'PY'
import json,sys,re
raw=open('output.txt').read()
m=re.search(r'\{.*\}',raw,re.S)
d=json.loads(m.group(0))
assert isinstance(d['approach'],str) and d['approach']
assert isinstance(d['files'],list) and d['files']
assert isinstance(d['api_contract'],str)
assert isinstance(d['test_plan'],str) and d['test_plan']
PY
