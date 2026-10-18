FOLDER: Incidents
# Incident Post-mortem & Prevention Loop

**Keywords:** auto-rollback, incident record, post-mortem, rolled-back commits blacklist, INCIDENTS_CHANNEL, team lesson, known-good deploy, rollback skip

## Overview

CXA-F012 turns every failed deploy into a why-plus-fix-the-root cycle instead of a revert-and-forget. After an auto-rollback or a deliberately skipped rollback it writes one durable `IncidentRecord` linking the failing commit to what it was rolled back to (or why it was not), blacklists the broken sha so self-healing never re-promotes it (`state.rolled_back_commits`), posts a human-readable post-mortem into the dedicated `#incidents` chat room (`INCIDENTS_CHANNEL`) and records a deduped team lesson via `add_lesson()`. It is best-effort throughout — none of its steps may break or stall a run — and runs at most once per deploy revision.

## How it works

All functions below are methods on `RunCycleUseCase`. The loop body lives in `crates/application/src/use_cases/cycle/ops.rs`; the trigger sits in `crates/application/src/use_cases/cycle/mod.rs`.

1. **Trigger.** In `cycle/mod.rs` (~line 1068) after this cycle's deploy step passes both gates (`deploy()` then `run_tests()`), if either gate failed (`deploy_bad || tests_bad`) the loop calls `attempt_rollback(reason)`, where reason is "deploy failed" or "tests failed". When both gates pass and something shipped (`deploy_ran_ok`) it calls `record_known_good(attempt_sha)` instead.
2. **Known-good pointer.** On success `record_known_good()` points git ref `LAST_GOOD_REF = "refs/coxagent/last-good"` at that sha and stores a new [`KnownGoodDeploy { sha, at, deploy_index, summary }`](crates/application/src/state) (`crates/application/src/state/ops.rs).
3. **Guard rails inside `attempt_rollback()`.** It is a no-op unless `config.deploy.auto_rollback`. It needs both the deploy adapter and git wired up; returns when there is no known-good yet (first-ever deploy); returns when already on the known-good sha (avoids a redundant second rollback when both gates fail one cycle). If the good deploy is older than `max_rollback_age_secs`, or its timestamp is unparseable (treated as stale), it routes through `record_rollback_skipped(... stale=true)`. If any file under any prefix in `migration_detection_paths` changed between good and failing sha (`migration_shipped_since()`), it routes through skipped with `migration_blocked=true`.
4. **The single retry.** Roll back into a dedicated secondary worktree at `rollback_worktree_path()` (named `<workdir-name>-rollback`, never touching the live work dir): freshly remove, then re-add, that worktree from the good sha via git worktree ops (outbound port). Deploy that tree and re-run the mandatory health gate (`verify_deploy_health()`). Exactly one attempt — if it also fails there is no loop; escalation happens via bug + notify.
5. **Record outcome.** `finish_rollback()` logs an activity row + an SM comment, sets `state.last_rollback = Some(RollbackStatus { ... })`, and on success preserves the failing forward deploy's health-check result so that diagnostic survives the rollback's own DeployStatus overwrite (COX-F005). It notifies kind "rollback_ok" or "rollback_failed"; on failure it files a High bug via `file_rollback_failed_bug()`. On success it then runs the post-mortem loop exactly once for this revision.
6. **Post-mortem loop (`post_mortem()`).** One atomic state mutation: insert the failed sha into `state.rolled_back_commits` (the blacklist), call `post_chat_in("SM", body, INCIDENTS_CHANNEL, ...)` where body carries mood "rolled-back" or "rolled-forward/stale", push an `IncidentRecord` (defined in `crates/application/src/state`) onto `state.incidents`, then drain any overflow past `MAX_INCIDENTS` (=12, oldest first). Afterward it notifies kind "incident_post_mortem" and records a deduped team lesson via [`add_lesson()`](crates/application/src/state/mod.rs).

Skipped rolls route through `record_rollback_skipped()`, which still writes `RollbackStatus { ok:false, stale|migration_blocked }`, notifies kind "rollback_blocked", and still runs post_mortem() so an incident is never silent.

## Usage

There is no F012-specific command or endpoint to call; you enable auto-rollback behaviour through project deploy configuration and observe results passively on the next failing cycle.

1. **Enable auto-rollback** for a project (default off). The loop is gated entirely on `config.deploy.auto_rollback`; set it in that project's deploy config (the same JSON/env surface that drives `host_port`, `health_check_timeout_secs`, etc.). With it off, a failed deploy still files its bug but no rollback or incident loop runs.
2. **Trigger naturally.** Every agent-cycle run goes through `RunCycleUseCase::run_cycle()`. When a shipped revision fails the health probe (`deploy_bad`) or post-deploy tests (`tests_bad`), rollback fires and post_mortem() runs.
3. **Watch `#incidents`.** The post-mortem body is posted with mood "rolled-back" / "rolled-forward/stale", e.g.:
   ```
   🩺 Post-mortem (deploy failed): <summary> → rolled-back to <short-sha> [<failed-sha>] — root cause filed as tracked work.
   ```
   It shows up in the web chat under the dynamically-listed channels (see /api/chat/channels).
4. **Read durable state.** The incident lives on as an entry in `state.incidents` plus the blacklist entry in `state.rolled_back_commits`; both persist with project state.

**Simulating without a real broken deploy:** unit/integration coverage drives this path with scripted adapters — see `cycle_tests.rs` (`crates/application/src/use_cases/cycle`) using [`ScriptedDeploy`](crates/application/src) and `SpyNotifier` to assert exactly one post-mortem into #incidents per successful rollback, e.g. `successful_rollback_produces_a_post_mortem_in_the_incidents_channel()`.

## Interface

The feature exposes no HTTP endpoint or CLI flag of its own — it is an internal behaviour of `RunCycleUseCase`. Its observable surface:

- **Notify kinds** (via `NotifierPort`): `"incident_post_mortem"`, plus the surrounding rollback events `"rollback_ok"`, `"rollback_failed"`, `"rollback_blocked"`.
- **State records** (`crates/application/src/state/mod.rs` + ops section):
  - [`IncidentRecord { at: String, reason: String, failed_sha: String, to_sha: String (default ""), ok: bool, summary: String, root_cause_ticket: Option<String> (default None), lesson: Option<String> (default None) }`](crates/application/src)
  - [`RollbackStatus { at, reason, to_sha, ok, summary, stale (default false), migration_blocked (default false) }`]
  - `KnownGoodDeploy { sha, at, deploy_index: u64, summary }`
- **Constants** (`crates/application/src/state/mod.rs`): `INCIDENTS_CHANNEL = "incidents"`, `MAX_INCIDENTS = 12`.
- **Git ref**: `LAST_GOOD_REF = "refs/coxagent/last-good"` (`cycle/mod.rs`) points at the last revision that passed both gates; rollback targets it.

## Configuration

All settings live in [`DeployConfig`](crates/application/src/config.rs) (`crates/application/src/config.rs`), loaded per project from its deploy config surface (JSON / env). Only `auto_rollback` gates the whole loop; the rest tune when a rollback is attempted vs skipped.

| Field | Default | Effect |
|-------|---------|--------|
| `deploy.auto_rollback` | `false` (opt-in) | When false, a failed deploy only files its bug — no rollback, no incident loop. |
| `deploy.max_rollback_age_secs` | `3600` | A known-good deploy older than this is "stale" and rollback is skipped (not attempted); an unparseable timestamp also counts as stale. |
| `deploy.migration_detection_paths` | `["migrations"]` | Repo-relative prefixes; if any changed file path starts with one between good and failing sha, rollback is skipped as migration-blocked. |
| `deploy.health_check_timeout_secs` | `60` | The mandatory post-deploy health gate that decides success/failure; drives both forward deploy and rollback verification. |

There are no environment variables specific to CXA-F012.

## Edge cases and limits

- **No prevention ticket is filed by post_mortem()**. The `IncidentRecord.root_cause_ticket` field exists but nothing ever sets it — it stays `None`. The root-cause bug is filed by the earlier failure handlers before rollback (`file_deploy_bug()` for "Deploy failing", `file_test_failure()` for test failures, both deduped on an open matching title), and a failed *rollback* files its own High bug via `file_rollback_failed_bug()` ("Rollback failed"). Do not read this page expecting the incident loop itself to create the fix ticket.
- **Best-effort throughout**: every state mutation and notify call discards errors (`let _ =`, `.ok()`), so a failure in this path never breaks or stalls a run — but it can also be silently dropped.
- **At most once per revision**: call sites ensure `post_mortem()` runs once; there is no internal dedupe counter against concurrent duplicate triggers within one cycle.
- **Rollback capped at one retry** — if the rollback deploy also fails there is no further loop, only escalation.
- **Silent when disabled**: with `auto_rollback` off, no incident is recorded at all (the deploy bug still is).
- **Blacklist & incidents are capped**: incidents drain oldest-first past 12; lessons are deduped and capped at 12 via `add_lesson()`.
- **Empty failed_sha**: if the failing sha was absent, `IncidentRecord.failed_sha` becomes `""`, and the blacklist insert is skipped (guarded non-empty).

## Code map

- `crates/application/src/use_cases/cycle/ops.rs` — the whole loop: `attempt_rollback()`, `record_known_good()`, `finish_rollback()`, `record_rollback_skipped()`, `post_mortem()`, plus helpers `migration_shipped_since()` / `rollback_worktree_path()` / `verify_deploy_health()` and bug filers `file_rollback_failed_bug()` / `file_deploy_bug()`.
- state field storage lives in `crates/application/src/state/mod.rs` (constants INCIDENTS_CHANNEL, MAX_INCIDENTS; ProjectState fields incidents, rolled_back_commits, last_good_deploy, last_rollback; methods post_chat_in(), add_lesson()).
- record types KnownGoodDeploy / RollbackStatus / IncidentRecord live in `crates/application/src/state/ops.rs`.
- config surface is DeployConfig in `crates/application/src/config.rs` (see Configuration); loading + default-stability regression coverage in `crates/app/src/config_load.rs`.
- behaviour tests: `crates/application/src/use_cases/cycle/cycle_tests.rs`, driving ScriptedDeploy + SpyNotifier to assert one #incidents post-mortem per successful rollback and a distinct notify kind.

## Related

- **COX-F005** — the mandatory post-deploy health check (`verify_deploy_health()`); its pass/fail decides whether rollback triggers, and a successful rollback preserves that failing health result into state (see finish_rollback comments).
- **Deployment / known-good pointer** — this loop is one consumer of `LAST_GOOD_REF` and `state.last_good_deploy`; related deployment-health docs cover the gate that feeds it.
- **Cycle deploy failure handling** — `file_deploy_bug()` / `file_test_failure()` file the root-cause bug at the trigger (before rollback), so CXA-F012's "prevention" rests on those tickets rather than a new one.
