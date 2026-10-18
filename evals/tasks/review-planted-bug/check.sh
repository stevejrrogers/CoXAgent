#!/usr/bin/env bash
# The planted borrow-while-iterating bug must be caught.
grep -q '"decision"[[:space:]]*:[[:space:]]*"request_changes"' output.txt
