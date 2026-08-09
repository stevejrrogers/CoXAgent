FOLDER: Engineering

# Optimistic Concurrency Control for Cross-Runner Store Saves

**Keywords:** state store, optimistic concurrency, revision, CAS, lost update, save_expecting, current_version, RestStateStore, SqlStateStore, PortError::Conflict

## Overview

CXA-F003 closes a lost-update hole in the project state store: when two runners (or one runner and an operator) work on the same project across process or machine boundaries, each writer that re-reads at write time could both succeed and silently clobber each other's changes. The fix adds a monotonic `revision` to the Postgres aggregate row and threads that version token through the port (`save_expecting`, `current_version`) and across REST so a stale writer is rejected with a conflict instead of overwriting newer data. It is for every adapter implementer of `StateStorePort` and any client that wants stale-write protection over REST.

## How it works

The durable aggregate lives as one JSONB row per project keyed by `project_id`, carrying a `revision BIGINT` that every successful write bumps atomically:

1. A caller reads state via `load()`, capturing its current revision through `current_version()`.
2. To persist guarded against staleness it calls `save_expecting(&state, Some(rev))`, where `rev` is exactly what that caller saw when it loaded.
3. On Postgres this becomes [`SqlStateStore::persist_at_revision`](crates/infrastructure/src/state/sql_store.rs)'s atomic UPSERT:
   ```
   INSERT INTO project_state (project_id, schema_version=..., revision=1, data=...) VALUES (...)
   ON CONFLICT (project_id) DO UPDATE
      SET data = EXCLUDED.data,
          schema_version = EXCLUDED.schema_version,
          revision = project_state.revision + 1,
          updated_at = now()
      WHERE project_state.revision = $4     -- expected token
   ```
   When zero rows match (the row moved past what this caller captured), it returns [`PortError::Conflict("state changed since last read")`](crates/application/src/error.rs). The guarded path never re-reads at write time — it trusts only what THIS caller loaded.
4. Backends without revision tracking keep today's behaviour: trait defaults fall through to plain [`save()`] and report no token (`current_version -> None`).

Cross-machine coordination around claims is separate but adjacent: [`SqlStateStore::claim_ticket`] serializes read-check-set-write inside one transaction with `SELECT ... FOR UPDATE`, so two machines racing on a backlog can never both win; leader/stage/worker leases route through Redis when attached.

The REST gateway ([store_rpc.rs](crates/presentation/src/server/store_rpc.rs)) exposes three relevant ops: `version` returns the current revision; `save` accepts an optional body field `revision`, forwards it via [`save_expecting`], and maps any resulting conflict to HTTP **409** so a REST-fronted runner retries its read-modify-write; older clients omitting the field keep legacy semantics (`None`, no guard).

Callers wanting retry-on-conflict use the free helper function [`mutate_state<S,F>`](crates/application/src/ports/outbound/state_store.rs), which loops up to 12 times: load, apply a mutator, save; on `PortError::Conflict` it reloads and re-applies against fresh state until convergence or exhaustion.


## Usage

A single actor saving normally:

```rust
let mut state = store.load().await?;
// ... mutate ...
store.save(&state).await?;           // legacy path; still validates + CASes internally
```

Two actors guarding against clobbering:

```rust
let rev_a = store.current_version().await?;        // Some(7)
// ... meanwhile B writes rev 8 ...
store.save_expecting(&my_state_from_7, Some(7)).await;
// => Err(PortError::Conflict("state changed since last read..."))
```

Retry loop using built-in convergence:

```rust
use coxagent_application::ports::outbound::{StateStorePort as _, mutate_state};
mutate_state(&store,
    |s| { s.set_desired(true); Ok(()) }).await?;
```

Over REST capture-then-guard:

```
POST /api/projects/<id>/store?op=version     -> { "revision": 7 }
POST /api/projects/<id>/store?op=save        body { "data": "<ProjectState JSON>", "revision": 7 }
# stale -> HTTP 409 { "conflict": "..." }; current -> HTTP 200 { "ok": true }
```

## Interface

On trait [`StateStorePort`](crates/application/src/ports/outbound/state_store.rs) plus the free helper defined beside it in the same file:

- `async fn load(&self) -> Result<ProjectState, PortError>`
- `async fn save(&self,&ProjectState) -> Result<(),PortError>` — legacy path.
- `async fn save_expecting(&self,&ProjectState ,Option<i64>) -> Result<(),PortError>` — NEW. CAS against caller-captured revision; default falls through to save(). Returns PortError::Conflict on staleness.
- `async fn current_version(&self) -> Result<Option<i64>,PortError>` — NEW. Current revision token for this aggregate; default returns None.
- Free helper `mutate_state<S,F>(&S store?, F mutator)` — up-to-12-retry convergence on conflict.

The dispatch enum adapter ([any_store.rs](crates/infrastructure/src/state/any_store.rs)) overrides all three methods by forwarding to whichever variant is active.

HTTP endpoint (`crates/presentation/src/server/store_rpc.rs`, route registered as `/api/projects/:pid/store`, POST):

| op | body fields | response |
|----|-------------|----------|
| load | – | full ProjectState |
| version | – | {"revision": i64} (absent-row projects report baseline 0) |
| save | data (JSON string), optional revision | {"ok": true}, or HTTP 409 {"conflict":"..."} when stale |

Other ops unchanged under this ticket: claim_ticket (returns won bool), acquire_leader / claim_stage / release_stage / heartbeat / workers / set_desired / get_desired / acquire_operator.

## Configuration

No new configuration flags were added by CXA-F003. Behaviour depends entirely on whether a backend tracks revisions:

- **Postgres** ([sql_store.rs](crates/infrastructure/src/state/sql_store.rs)) — always tracked via the row-count predicate in persist_at_revision.
- **REST** ([rest_store.rs](crates/infrastructure/src/state/rest_store.rs)) — forwards captured revisions and maps remote HTTP CONFLICT to PortError::Conflict.
- **JSON** ([json_store.rs](crates/infrastructure/src/state/json_store.rs)) — does NOT override either new method; uses trait defaults (no guard available); remains best-effort backup only.
- Schema migration is idempotent SQL run on connect in migrate(); no user config beyond existing DSN wiring (`COXAGENT_TEST_PG_DSN` gates integration tests).

## Edge cases and limits

It deliberately does NOT cover:
- Rows not yet written expose baseline revision 0 matching persist_at_revision's first insert landing at revision 1.
- Writers that pass only data without capturing a version get no cross-boundary protection — they hit legacy re-read-at-write semantics where two simultaneous writers within one connection can both succeed ("lost update"). Use capture-and-guard or mutate_state for full protection.
- Revision tokens are per-project monotonic counters within one backend lineage only — there is no tombstone history or audit trail of who advanced which revision.
- Coordination leases rely on TTL renewal expiry rather than fencing tokens (no Lamport/fencing guarantee against a long-suspended holder).
- Stale writes fail closed with Conflict rather than erroring broadly or hanging; backends without revisions degrade silently to unguarded saves rather than failing.

All decisions remain pure functions over DB results behind ports — no direct IO enters application logic regardless of which port branch is chosen.

## Code map

These are the real files implementing CXA-F003 (all touched by commit "feat(CXA-F003): Optimistic concurrency control for cross-runner store saves"):

crates/application/src/ports/outbound/mod.rst omitted line --- list follows:
