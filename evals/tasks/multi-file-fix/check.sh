#!/usr/bin/env bash
grep -q 'assert_eq!(s.get("k", 10), None);' store.rs || { echo test-tampered; exit 1; }
rustc --test store.rs -o /tmp/eval_mff 2>/dev/null && /tmp/eval_mff >/dev/null 2>&1
