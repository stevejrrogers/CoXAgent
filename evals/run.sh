#!/usr/bin/env bash
# evals/run.sh — the fixed yardstick for agent quality.
#
# Runs every golden task in evals/tasks/ against an engine CLI and scores
# pass/fail with each task's own deterministic check.sh. Results append to
# evals/results/<date>-<engine>.json so a prompt or model change has a number,
# not a feeling.
#
# Usage: evals/run.sh [engine]      # engine: claude (default) | opencode
# Each task dir: prompt.md (the task), check.sh (exit 0 = pass),
# optional files/ (copied into the task's scratch dir; the engine runs there).
set -u

ENGINE="${1:-claude}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAMP="$(date '+%Y-%m-%d-%H%M')"
OUT="$ROOT/results/$STAMP-$ENGINE.json"
pass=0; fail=0; results=""

run_engine() { # $1=workdir $2=prompt-file → stdout captured by caller
  case "$ENGINE" in
    claude)   (cd "$1" && claude -p --dangerously-skip-permissions "$(cat "$2")" 2>/dev/null) ;;
    opencode) (cd "$1" && opencode run "$(cat "$2")" 2>/dev/null) ;;
    *) echo "unknown engine $ENGINE" >&2; exit 2 ;;
  esac
}

for task in "$ROOT"/tasks/*/; do
  name="$(basename "$task")"
  work="$(mktemp -d)"
  [ -d "$task/files" ] && cp -R "$task/files/." "$work/"
  echo "▶ $name"
  run_engine "$work" "$task/prompt.md" > "$work/output.txt"
  if (cd "$work" && bash "$task/check.sh" >/dev/null 2>&1); then
    echo "  ✅ pass"; pass=$((pass+1)); verdict=true
  else
    echo "  ❌ FAIL"; fail=$((fail+1)); verdict=false
  fi
  results="$results{\"task\":\"$name\",\"pass\":$verdict},"
  rm -rf "$work"
done

total=$((pass+fail))
echo "{\"at\":\"$STAMP\",\"engine\":\"$ENGINE\",\"pass\":$pass,\"total\":$total,\"tasks\":[${results%,}]}" > "$OUT"
echo
echo "score: $pass/$total  →  $OUT"
[ "$fail" -eq 0 ]
