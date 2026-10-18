#!/usr/bin/env bash
grep -qiE "port|address" output.txt && grep -qiE "already|in use|another process|đang" output.txt
