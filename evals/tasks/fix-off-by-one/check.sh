#!/usr/bin/env bash
# Pass if the (unchanged) test now passes and the test block was not edited.
grep -q 'assert!(!in_bounds(1, 1));' lib.rs || { echo "test was modified"; exit 1; }
rustc --test lib.rs -o /tmp/eval_obo 2>/dev/null && /tmp/eval_obo >/dev/null 2>&1
