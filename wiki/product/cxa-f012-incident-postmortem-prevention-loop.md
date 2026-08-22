FOLDER: -
# Incident post-mortem & prevention loop after rollback

**Keywords:** auto-rollback, incident post-mortem, last known good deploy, rolled-back commits blacklist, incidents channel, team lesson, migration detection path, rollback age window, health check gate, rollback blocked

## Overview

CXA-F012 is the durable learning half of auto-rollback: every time the run loop redeploys (or deliberately refuses to redeploy) a previously healthy build after a deploy/test failure it writes one inspection-grade `IncidentRecord`, posts a human-readable post-mortem into an **`incidents`** chat room — not `general` or `agents`, where nobody watching for outages looks — emits a distinguishable NotifierPort event (**`incident_post_mortem`**), and appends a team lesson so each outage becomes a "why + fix-the-root" cycle instead of revert-and-forget. It serves operators watching for outages and agents that must prove every broken revision became tracked work.

The loop does **not** itself file the root-cause ticket: the failing forward path already filed its bug via `file_deploy_bug` / `file_test_failure` before rollback runs; only when the rollback *mechanism* fails is an extra High bug filed via `file_rollback_failed_bug`. The incident record's prevention-ticket link stays empty by design — see Edge cases.

## How it works

Flow lives in `RunCycleUseCase::run_cycle()` (`crates/application/src/use_cases/cycle/mod.rs`) with all deploy/post-mortem logic in its sibling module file (`.../cycle/ops.rs`).

1. **Failure triggers rollback.** After deploy + tests (mod.rs:1055-1080), if either gate failed (`deploy_bad || tests_bad`) with reason `"deploy failed"` or `"tests failed"`, the cycle calls `attempt_rollback(reason, attempt_sha)` (ops.rs:160). A red suite already filed its high-priority bug via `file_deploy_bug` / `file_test_failure`.
2. **Early exits keep today's behavior.** If auto-rollback is off (`config.deploy.auto_rollback == false`) or there is no prior known-good deploy (`state.last_good_deploy == None`, first-ever deploy) or we are already on the known-good sha (both gates failed this cycle), it returns without touching anything.
3. **Safety skips.** If the last good deploy is older than `max_rollback_age_secs`, or any changed path under any prefix in [`migration_detection_paths`] shipped since it ([WHY]), roll back skipped deliberately noting why.
