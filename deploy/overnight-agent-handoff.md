# Autonomous Overnight Agent — Operating Handoff

You are running CoXAgent's overnight autonomous loop. Your job: keep the hub
healthy and deploy code fixes when the runner hits NEW bugs, all without user
prompting. Work in autonomous cycles until morning.

## Verification checklist (impacts before editing)
- MUST run impact analysis on any symbol before editing it (GitNexus MCP).
- NEVER edit a modified (M) file without reading its full `git diff` first.
- After changes, run `detect_changes()` before committing.

## Repo topology
- Worktree (code + build): `/private/tmp/coxa-agent-main` (detached HEAD of
  origin/main, contains fix #139). Build here, NEVER in the live repo.
- Live bundle binary: `/Users/luton/Projects/CoXAgent/desktop/build/CoXAgent.app/Contents/MacOS/cox-server`
- Deploy helper: `/Users/luton/Projects/CoXAgent/deploy/deploy-cxa.sh` (does
  build -> backup -> swap -> codesign -> optional restart -> verify). Use it.
- Hub is managed by the desktop app shell. NEVER bind port 4000 yourself.
  Restart by TERM-ing the hub PID; the shell respawns it.

## Routine monitoring (already handled by watchdog-cxa.sh, do NOT duplicate)
- The shell watchdog `/Users/luton/Projects/CoXAgent/deploy/watchdog-cxa.sh`
  (detached) auto-restarts a dead hub and auto-cleans rejected 'Debt sweep'
  tickets every 60s. Logs to `deploy/watchdog.log`.
- YOU focus on what the shell watchdog cannot do: code-level NEW bugs.

## Operating loop (every ~15 min)
1. `tail deploy/watchdog.log` + `curl http://127.0.0.1:4000/api/health`.
2. If a log line reads `HEAL-FAILED` (hub down, restart failed after ~30s):
   investigate why — the app shell may be dead. Re-run restart, or re-queue.
3. If a log line reads `REGRESSION-PERSIST` (rejected 'Debt sweep' persists
   after auto-clean): the code path is actively re-filing. This is a code bug:
   - Read `crates/application/src/use_cases/cycle/backlog.rs`, `has_rejected_sweep`.
   - Impact on the guard function; fix the re-file path; add/adjust a test.
   - Rebuild + deploy via `deploy/deploy-cxa.sh <worktree> --restart`.
   - Verify 0 rejected 'Debt sweep' after a few loops.
4. Check runner health: count opencode workers (`pgrep -f opencode`). If 0 for
   a long stretch with hub up, the runner loop may be stuck -> restart hub.
5. Scan live store for other anomaly tickets via the store API (see auth below).
   Only act on genuine bugs, not noise. Do not over-engineer.

## Auth (for store RPC / metrics), see runbook
- Login: `POST /api/auth/login` {root + `/Users/luton/CoXAgent/admin-password`} ->
  HttpOnly `cox_session` cookie (bare 64-char token, NO `#HttpOnly_` prefix).
- Store RPC: `POST /api/projects/cxa/store?op=save` with `{revision, data}`;
  stale revision -> 409 (retry with fresh `?op=version`).
- Health: `curl http://127.0.0.1:4000/api/health` -> `{"status":"ok"}`.
- Never expose credentials in logs.

## Escalation
- If you hit a bug you cannot root-cause or that needs schema changes beyond
  scope, write a precise handoff note to
  `/Users/luton/Projects/CoXAgent/deploy/handoff-<issue>.md` describing the
  symptom, evidence, and attempted fixes, then continue the loop on other items.
- Always keep the hub RUNNING (deploy only working builds; validate build first).

## Constraint
- Do not touch GitHub Actions / CI (billing). Deploy locally + restart hub.
- Keep each edit surgical; run the smallest targeted `cargo test -p <crate>`.
- Report a concise summary of what you fixed/deployed at the end, and log
  every deploy to `deploy/deploy.log`.
