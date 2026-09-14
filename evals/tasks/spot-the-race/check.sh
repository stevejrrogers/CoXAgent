#!/usr/bin/env bash
grep -qi "HITS" output.txt && grep -qiE "race|atomic|unsynchron|concurrent|data race|torn|lost update" output.txt
