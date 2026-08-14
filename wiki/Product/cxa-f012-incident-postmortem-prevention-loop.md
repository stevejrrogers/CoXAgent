FOLDER: Incidents
# Incident Post-mortem & Prevention Loop

**Keywords:** auto-rollback, rollback, incident record, post-mortem, root cause prevention ticket, blacklist, known-good deploy

## How it works

The flow lives in the cycle's deploy phase on RunCycleUseCase in crates/application/src/use_cases/cycle/ops.rs.

1. Trigger. In cycle/mod.rs around line 1068 the loop calls attempt_rollback(reason, failed_sha, report) whenever either deploy_bad or tests_bad is set after this cycle's deploy step; reason is "deploy failed" or "tests failed". When both gates pass and something shipped (deploy_ran_ok) it calls record_known_good() instead.
2. Guard rails inside attempt_rollback(): no-op unless config.deploy.auto_rollback; needs both state.last_good_deploy and git wired up; returns if there is nothing to roll back to yet; returns if already on the known-good sha; records skipped-stale when older than max_rollback_age_secs; records skipped-migration-blocked when migration_shipped_since() sees any prefix change under migration_detection_paths.
3. Attempt exactly once in a dedicated secondary worktree (rollback_worktree_path()) that was freshly recreated from the known-good sha; deploy it and re-run the mandatory health gate (verify_deploy_health). One retry cap: a second failure stops here.
4. finish_rollback() logs an activity row and an SM comment, sets state.last_rollback = Some(RollbackStatus { ok, ... }), and on success preserves the failing forward deploy's HealthCheckResult so that diagnostic survives the rollback's own DeployStatus overwrite (COX-F005). It notifies the channel with kind "rollback_ok" or "rollback_failed", and on failure files a High bug via file_rollback_failed_bug().
5. On a successful rollback finish_rollback() calls post_mortem() exactly once per revision. One atomic state mutation: insert the failed sha into state.rolled_back_commits (the blacklist), post_chat_in the post-mortem body to channel INCIDENTS_CHANNEL ("incidents"), push an IncidentRecord onto state.incidents, then drain any overflow past MAX_INCIDENTS (12). Afterward it notifies with kind "incident_post_mortem" and records a deduped team lesson via add_lesson(). The IncidentRecord's root_cause_ticket and lesson fields start as None.

Skipped rolls route through record_rollback_skipped(), which still writes state.last_rollback = Some(RollbackStatus { ok: false, stale or migration_blocked set }), notifies kind "rollback_blocked", and still runs post_mortem() so the incident is never silent.

## Overview

CXA-F012 turns every failed deploy into a why-plus-fix-the-root cycle instead of a revert-and-forget. After an auto-rollback or a deliberately skipped rollback it writes one durable incident record linking the failing commit to what it was rolled back to (or why it was not), blacklists the broken sha so self-healing never re-promotes it, posts a human-readable post-mortem into a dedicated incidents room and records a deduped team lesson plus files or links a root-cause prevention ticket. It is best-effort throughout and runs at most once per deploy revision.
