FOLDER: Engineering

# Optimistic Concurrency Control for Cross-Runner Store Saves

**Keywords:** state store, optimistic concurrency, revision, CAS, lost update, save_expecting, current_version, mutate_state, RestStateStore, SqlStateStore, PortError::Conflict

## Overview

CXA-F003 closes a lost-update hole in the project state store: when two runners (or one runner and an operator) work on the same project across process or machine boundaries, each writer that re-reads at write time could both succeed and silently clobber each other's changes. The fix adds a monotonic `revision` to the Postgres aggregate row and threads that version token through the port (`save_expecting`, `current_version`) and across REST so a stale writer is rejected with a conflict instead of overwriting newer data. It is for every adapter implementer of [`StateStorePort`](crates/application/src/ports/outbound/state_store.rs) and any client that wants stale-write protection over REST.

## How it works

Since CXA-C019b the durable aggregate lives as one JSONB row per bounded-context shard (the [`ShardKind`](crates/application/src/state/shards.rs) vocabulary: `work`, `social`, `docs`, `governance`, `ops`) in `project_state_shard`, plus one `project_state_head` row per project carrying the single optimistic `revision BIGINT` that every successful write bumps atomically. Shard payloads are stored as externally-tagged [`ShardData`](crates/application/src/state/shards.rs) JSON, so each row is self-describing; each shard row also carries its own `revision` write counter (per-shard observability: which contexts a project actually writes). The five shards are a complete, disjoint partition of `ProjectState` enforced by the C019a seam (`into_shards`/`from_shards`).

1. A caller reads state via `load()`, capturing its current revision through `current_version()`.
2. To persist guarded against staleness it calls `save_expecting(&state, Some(rev))`, where `rev` is exactly what that caller saw when it loaded.
3. On Postgres this becomes [`SqlStateStore::persist_at_revision`](crates/infrastructure/src/state/sql_store.rs)'s two-phase write in ONE transaction: first the guarded head CAS (from [`tombstone.rs`](crates/infrastructure/src/state/tombstone.rs)):
   ```sql
   INSERT INTO project_state_head (project_id, schema_version, revision)
   SELECT $1, $2, 1
    WHERE NOT EXISTS (SELECT 1 FROM project_tombstone WHERE project_id = $1)
      AND NOT EXISTS (SELECT 1 FROM project_state WHERE project_id = $1)
   ON CONFLICT (project_id) DO UPDATE
       SET schema_version = EXCLUDED.schema_version,
           revision = project_state_head.revision + 1,
           updated_at = now()
       WHERE project_state_head.revision = $3     -- expected token
         AND NOT EXISTS (SELECT 1 FROM project_tombstone WHERE project_id = $1)
   ```
   then — only for the winner — an upsert of each shard whose serialized payload changed. When zero rows match (the row moved past what this caller captured), it returns [`PortError::Conflict("state changed since last read")`](crates/application/src/error.rs) and writes nothing. The guarded path never re-reads at write time — it trusts only what THIS caller loaded.
4. Backends without revision tracking keep today's behaviour: trait defaults fall through to plain [`save()`] and report no token (`current_version -> None`).

Cross-machine coordination around claims is separate but adjacent: [`SqlStateStore::claim_ticket`] takes the head row's lock first (the same lock order every writer uses, so claim and shard-diff save never deadlock) and then locks ONLY the Tickets shard row (`shard = 'work'`) `FOR UPDATE` — the whole-aggregate row lock this method used under CXA-C019a and earlier is exactly the write contention C019b removes. The revision bump, the ticket mutation and the shard-row update commit in one transaction, so two machines racing on a backlog can never both win; leader/stage/worker leases route through Redis when attached.

**Legacy migration (CXA-C019b).** A pre-C019b project was one JSONB row in `project_state`. On the first post-upgrade connect (and lazily on the first save of any store that connected before the row appeared) the adapter migrates it inside one transaction: split via `into_shards` → write the five shard rows → freeze the original full JSON as `shard = '_legacy'` (write-once, for recovery/rollback tooling) → sync the head revision UP to the legacy row's (never backward) → tombstone the legacy row. Crash-safe (one tx) and concurrent-safe (the legacy row is locked; a second migrator finds it gone). A legacy row that is not decodable, or whose `schema_version` is newer than this binary supports, is left untouched — `load()` keeps returning today's exact error for it. Mixed-binary caveat: an OLD binary writing the single-blob row after a project migrated leaves a residue row; `load()` prefers it only while its revision is strictly newer than the head (that write is the latest state), and any surviving residue is tombstoned by the next winning save. `current_version()` falls back to the legacy row's revision while the head row is absent, so REST `op=version` never reports a rewound revision during the window.

The local JSON mirror stays a whole-state backup file (format unchanged, so seed/restore keeps working) — every save and every claim mirrors the reassembled aggregate.

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
| load | – | full ProjectState (reassembled from shard rows) |
| version | – | {"revision": i64} (absent-row projects report baseline 0) |
| save | data (JSON string), optional revision | {"ok": true}, or HTTP 409 {"conflict":"..."} when stale |

Other ops unchanged under this ticket: claim_ticket (returns won bool), acquire_leader / claim_stage / release_stage / heartbeat / workers / set_desired / get_desired / acquire_operator.

## Configuration

No new configuration flags were added by CXA-F003. Behaviour depends entirely on whether a backend tracks revisions:

- **Postgres** ([sql_store.rs](crates/infrastructure/src/state/sql_store.rs)) — always tracked via the row-count predicate in persist_at_revision.
- **REST** ([rest_store.rs](crates/infrastructure/src/state/rest_store.rs)) — forwards captured revisions and maps remote HTTP CONFLICT to PortError::Conflict.
- **JSON** ([json_store.rs](crates/infrastructure/src/state/json_store.rs)) — does NOT override either new method; uses trait defaults (no guard available); remains best-effort backup only.
- Schema migration is idempotent SQL run on connect in migrate() (the shard table, the head table and its legacy backfill seed since CXA-C019b); no user config beyond existing DSN wiring (the DB-backed tests provision their own ephemeral Postgres via the shared compose fixture, CXA-F327).

## Edge cases and limits

It deliberately does NOT cover:
- Rows not yet written expose baseline revision 0 matching persist_at_revision's first head insert landing at revision 1.
- Writers that pass only data without capturing a version get no cross-boundary protection — they hit legacy re-read-at-write semantics where two simultaneous writers within one connection can both succeed ("lost update"). Use capture-and-guard or mutate_state for full protection.
- Revision tokens are per-project monotonic counters within one backend lineage only — there is no tombstone history or audit trail of who advanced which revision.
- Coordination leases rely on TTL renewal expiry rather than fencing tokens (no Lamport/fencing guarantee against a long-suspended holder).
- Stale writes fail closed with Conflict rather than erroring broadly or hanging; backends without revisions degrade silently to unguarded saves rather than failing.
- The sharded layout is adapter-internal (CXA-C019b): no port, HTTP or config change. `'_legacy'` and any unrecognized shard labels are ignored by `load()`'s join; a shard row whose payload disagrees with its label is refused as Corrupt rather than trusted.

All decisions remain pure functions over DB results behind ports — no direct IO enters application logic regardless of which port branch is chosen.

## Code map

The real files implementing CXA-F003:

crates/application/src/ports/outbound/state_store.rs — the [`StateStorePort`] trait: `save_expecting`, `current_version` (with default no-op fallbacks), and the free retry helper `mutate_state`.
crates/application/src/error.rs — [`PortError::Conflict`] variant returned on stale writes.
crates/application/src/state/shards.rs — the C019a shard seam: `ShardKind`, the per-context payload structs, `into_shards`/`from_shards`.
crates/infrastructure/src/state/sql_store.rs — Postgres adapter: sharded-rows storage (CXA-C019b), guarded head CAS + changed-shard diff write in `persist_at_revision`, shard-scoped `claim_ticket`, legacy single-blob migration.
crates/infrastructure/src/state/tombstone.rs — the durable delete tombstone plus the guarded head CAS every write filters through.
crates/infrastructure/src/state/json_store.rs — file-backed fallback; does NOT override the new methods (no guard).
crates/infrastructure/src/state/rest_store.rs — REST client adapter forwarding captured revisions and mapping remote HTTP 409 CONFLICT to PortError::Conflict.
crates/infrastructure/src/state/mod.rs — re-exports of the state-store module surface.
crates/presentation/src/server/store_rpc.rs — REST gateway `/api/projects/:pid/store`: op=load / version / save with optional revision field.

Tests:
crates/infrastructure/tests/sql_store_contract.rs — the port contract plus the CXA-C019b shard contract (legacy migration, per-shard round-trip and no-rewrite, shard-scoped claim lock, conflict writes nothing, gate refusal writes nothing), each against its own ephemeral Postgres from the shared compose fixture (CXA-F327; the COXAGENT_TEST_PG_DSN export is refused by policy).
crates/infrastructure/tests/sql_store_sharded_rows_f300_tdd.rs — the F300 source guards (pure, always run).
crates/infrastructure/tests/distributed_coord.rs — cross-machine claim coordination test (adjacent lease behaviour, not revision CAS).

## Related

- CXA-F001 / state-store REST integration work in `.claude/handoff-rest-runner.md` — this ticket's read-modify-write retry lands on top of that transport; read the hand-off before touching store_rpc.rs or auth hardening.
- [`StateStorePort`](crates/application/src/ports/outbound/state_store.rs) shares its file with the Redis coordinator wiring; claim/stage/leader leases live in crates/infrastructure/src/state/{redis_coord.rs, any_store.rs}.
- API gateway route docs and shape checks for `/api/projects/:pid/store` are governed by gitnexus route_map / api_impact (`gitnexus://repo/cxa/...`) since store_rpc consumers span presentation and application.
- The hexagonal IO-discipline guard (crates/app/tests/hexagonal_gate.rs) continues to enforce that all adapters above stay behind ports.

