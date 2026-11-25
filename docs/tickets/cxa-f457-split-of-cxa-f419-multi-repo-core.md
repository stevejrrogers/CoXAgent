# CXA-F457 — Split CXA-F419 into three ordered child tickets (multi-repo core)

**Status:** split recorded, awaiting SM ratification · **Parent:** CXA-F419 (tracking epic only)
**Design source of truth:** `docs/CXA-F420-multi-repo-projects-spec.md` (approved design,
Steve 2026-10-16) — rules R1/R2/R3 are quoted from it and traced to code that exists
on this branch today; nothing in the split invents new state.

## 0. Naming & ordering contract

The F419 parent id already carries `.a/.b/.c` children on sibling branch
`feat/CXA-F419` (commits 90ef802d/9c9a9cf6 — SEC-12CF60 viewer/export/retention,
prompt-injection-sanitized body). To avoid colliding with those ids in history
records, the multi-repo slice chain uses `.r1/.r2/.r3`.

| Order | Id | Slice | Deps |
|---|---|---|---|
| 1 | **CXA-F419.r1** | Silent legacy single-repo → `services[{name:'main'}]` schema compatibility | none |
| 2 | **CXA-F419.r2** | `service_tag`-driven worktree selection and gating | blocked-by r1 until r1 done |
| 3 | **CXA-F419.r3** | Per-service PR queue / WIP isolation | blocked-by r2 until r2 done |

Execution order is exactly 1→2→3. **SM must ratify the ids before r1 enters a
cycle**: `.r1/.r2/.r3` become load-bearing in cycle/history records once minted.

Every AC currently on the F419 multi-repo parent lands under at least one child
below; no AC appears under two siblings.

---

## 1. CXA-F419.r1 — Silent legacy single-repo schema compatibility

### WHAT & WHY
Legacy projects have no `services[]` key at all. They must keep working
byte-for-byte: one implicit service named `main`, synthesized in memory on
load, no migration, no registry writes, no new serialized keys. This is rule
R1 of the approved spec ("migration is silent").

### Seam (evidence in tree today)
- `crates/application/src/config.rs:1081` — `Config.services` field doc:
  *"Code services this project owns (CXA-F420). Empty = legacy single-repo"*;
  synthesis entry point `Config::effective_services(work_dir)` is already the
  read-path used by `crates/app/src/builders.rs:473`.
- Serialization stays untouched (`#[serde(default)]` + no new emitted keys),
  so existing `coxagent.json` layouts remain valid input.

### Contract for implementers
- `Config::effective_services` / `Config::resolve_service` keep their
  signatures and gain a guaranteed non-empty active set via synthesized
  `ServiceConfig { name: "main", status: "active" }` when `services` is empty.
- Read path only: zero writes, no `registry.json` writes, no state-schema or
  DB changes, rollback = revert commit.
- Exactly one info line (not silence, not a banner) when synthesis happens, so
  operators know why `main` appeared; dashboards and golden e2e screenshots
  for legacy configs stay byte-identical.

### Test plan (pure functions over real types — `config.rs` unit tests)
- legacy config (`services: []`) → exactly one synthesized `main` service,
  `status == "active"`;
- declared-services config → untouched, no synthesis;
- serialization round-trip emits no `services` key for legacy configs;
- exactly-one-info-line empty state asserted.

### Non-functionals
Attribution on every surfaced state (the info line names the cause);
no destructive ops; zero-config legacy projects byte-for-byte unchanged.
Child closes → parent F419 drops its "open tracking" hat for this slice.

---

## 2. CXA-F419.r2 — service_tag-driven worktree selection and gating

### WHAT & WHY
Ticket routing today degrades unsafely for multi-service projects: the tagged
ticket must land on *its* service's worktree and gates, and an untagged ticket
must go to the default (first active) service without stranding. The field is
already load-bearing since CXA-F420b ("drop invented service tags at mint",
commit 02741d5b).

### Seam (evidence in tree today)
- `crates/domain/src/ticket.rs:114` — `service_tag: Option<String>` on the
  `Ticket` aggregate.
- `crates/application/src/prompts.rs:184` (region) — resolve/degrade path the
  claim flow already calls (`Config::resolve_service`, tag → first-active →
  unknown-tag-degrades → paused-never-resolves).
- Consumed by the per-run wiring in `use_cases/cycle/wiring.rs` (`with_service`
  already threaded per-run), so this slice makes that decision explicit and
  testable instead of inline.

### Contract for implementers
- New pure fn in application:
  `route_ticket(ticket.service_tag, &[ServiceConfig]) -> Result<ServiceRef, ParkedReason{service_status, why}>`
  consumed by the claim path in `use_cases/cycle/wiring.rs`; hexagonal-gate
  clean (adapter does IO only — worktree checkout stays infrastructure).
- No port-trait breaking change; routing result carries attribution
  (`ParkedReason` says which service paused and why the ticket parked) — depth
  bar: off/parked ≠ silent.

### Test plan (table tests over the pure fn; hexagonal gate stays green)
- tagged hub vs harxes routes to the right service;
- untagged defaults to first active;
- unknown tag degrades to default, never strands;
- tagged-but-paused parks **with attribution** (`why` names the paused service).

---

## 3. CXA-F419.r3 — Per-service PR queue / WIP isolation

### WHAT & WHY
With ≥2 services, budgets, slot pools and log namespaces must not let one
service starve another, while single-service projects see zero behavioural
change.

### Seam (evidence in tree today)
- `crates/app/src/builders.rs:469` — the inline **CXA-F420a.2** carve-out
  builds per-service slot pools from `effective_services(...)` filtered to
  `status == "active"`; proves this seam has no home yet — r3 generalizes it.
- Budget/port surfaces keyed per service; `wiring.rs::with_service` threading
  generalizes across cycle steps.

### Contract for implementers
- `WorktreePool` / PR-budget ports become keyed by service name — no port-trait
  breaking change for single-service callers (key falls back to the single
  active service / `main`).
- Per-service counters are derived, not stored; reuse existing `service_tag`
  column and per-project `state.json` entries; no persisted-field additions;
  rollback trivially 'revert commit'.

### Test plan
- concurrency test with ≥2 services asserting no cross-service starvation
  (each pool progresses independently);
- golden-screenshot assertion that single-service dashboards render
  byte-identically to pre-r3 output (per-service counts appear only when
  ≥2 services exist).

---

## 4. Scope preservation & closure rule

- All original CXA-F419 acceptance criteria appear above under r1/r2/r3, each
  exactly once — none duplicated.
- Parent CXA-F419 stays open as a tracking epic only and **closes
  automatically once r1 ships** (its remaining governance scope is discharged
  by the ratified split); closing the parent while any child remains open is
  prevented, with the confirmation naming the unfinished child ids explicitly
  (`CXA-F419.r2`, `CXA-F419.r3`).
- Reordering children is allowed only where dependencies allow: attempting to
  move r2 before r1 (or r3 before r2) is refused with an explanatory message
  naming the dependency, never silently saved.
- No external API changes in the split itself; children carry the contracts
  above. Full playwright suite unaffected by this docs-only ticket.
