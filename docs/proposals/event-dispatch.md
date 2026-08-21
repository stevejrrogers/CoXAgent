# Event Dispatch: from one loop to reconcile + ceremony clock

Status: **approved 2026-08-20** (Steve). Implementation: P1/P2-lite by SRE (this doc's commit series), P3/P4 by agents with this doc as spec.

## One sentence

Split the single agent loop in two: **work** (design, dev, review, test, docs) moves to a
Kubernetes-style **reconcile** model — workers ask the store "what is waiting?" and take a
**lease** to do it — while **rhythm** (sprint, sentinel, release cut, hygiene, scorecard)
stays on the existing cycle, which shrinks into a ceremony clock.

## Why (evidence from the first dogfood week)

Most of the week's structural bugs were patches AROUND the loop, not isolated defects:

- **review-first**: a ripe PR waited behind the whole tail of the cycle; we moved review to
  the top. Patching order inside the loop instead of reacting to the event "a PR opened".
- **four separate "decouple X from the cycle counter" fixes** (10-minute sprints, debt
  cadence, docs budget, cadence config) — everything was tied to one counter that spins
  faster as the loop gets cheaper (cycles shrank ~30min → ~90s).
- **two bugs from leader-lease rotation across workers** (scorecard numbering, version
  mirror ping-pong) — the loop was never designed for a fleet.
- **reaction latency = cycle length**, always.

## The model: reconcile, NOT a message queue

A real queue imports a family of failure modes (lost message = lost work, duplicate
delivery, ordering, poison messages, one more system to operate). The correct model:

- **The store is the single source of truth.** "The queue" is a *query over state*
  ("which tickets are Ready with no dev claim? which PR heads have no review at this
  sha?"). There is no physical queue to corrupt.
- **A lease is the unit of work.** Atomic, TTL'd, per-ticket/per-PR — exactly the
  stage-claims already running in production.
- **Events are knocks on the door**, never the truth: a forge webhook or a store change
  only makes the scanner ask *sooner*. A lost event is harmless — the next scan sees the
  same fact. Polling is not a fallback; it is half of the reconcile model.
- **One lease valve** carries every scheduling policy that is currently scattered across
  eight phases: WIP limits, the cost gate, quiet hours, priority (bugs > features, aging
  against starvation), engine-down canary throttling. The valve is a pure function —
  table-testable.

## Job table

| Job          | Trigger (derived from state)              | Lease (already exists)        |
|--------------|-------------------------------------------|-------------------------------|
| `review_pr`  | PR open with unreviewed head sha           | per-PR + head-sha guard       |
| `design`     | ticket Pending without technical design    | stage claim `sa`              |
| `dev`        | ticket Ready / bug Open (in sprint scope)  | stage claim `dev`             |
| `test`       | ticket Fixed with no evidence              | stage claim `test`            |
| `merge_sync` | PR merged, not yet synced                  | `seen_merged_prs` guard       |
| `docs`       | ticket Done + daily budget left            | per-page cooldown             |
| `ba_intake`  | backlog below threshold                    | daily claim                   |

## Failure semantics (each row is a mechanism already in production)

| Failure                | Behaviour                                                        |
|------------------------|------------------------------------------------------------------|
| worker dies mid-job    | lease TTL expires → next scan re-derives the work → another worker |
| partial work in tree   | tree hygiene + BRIEF + failover-continuation: successor continues |
| poison job             | attempt-failures ladder: 3 strikes → park → SM escalation → human  |
| duplicate execution    | atomic leases; review by head-sha; merge sync by seen_merged      |
| lost event/webhook     | harmless by construction — next scan sees the state               |
| engine down            | valve closes for engine jobs, issues ONE canary lease             |
| overload               | the valve IS backpressure (WIP, budget, quiet hours)              |

## Invariants that do not move

landed-proof close, fix-on-fix brake, human-eyes holds, governance gate, scorecard,
budget metering, RBAC, tree hygiene. Same predicates; they attach to the job boundary
instead of the phase boundary.

## Rollout

- **P1 — dedicated review runner** (SRE, shipped with this doc): a separate loop runs
  review + PR feedback + forge hygiene every ~90s, reusing the full existing gate stack.
  A one-hour DEV phase no longer delays a ripe PR by a cycle. Duplicate-review safety:
  head-sha review guard + per-PR claim.
- **P2-lite — workers stop contending for leader** (SRE, shipped with this doc): worker
  slots never acquire the leader lease (the source of the scorecard-numbering and
  version-ping-pong bugs). The hub leader is slot 0; the app shell respawns it on death.
  Headless multi-machine runs keep lease election (env-gated) since no shell guards them.
- **P2 — workers run a job loop** (agents, spec = this doc): replace the worker's walk
  through `run_cycle` with: scan → lease → execute → repeat. Breaker/canary counting
  moves from cycles to job outcomes (same evidence-of-life predicate).
- **P3 — real knocks** (agents): forge webhook + store watch wake the scanner instantly;
  polling remains as the reconcile floor.
- **P4 — the valve learns full policy** (agents): priority with aging, quiet hours, cost,
  WIP — one pure function with property tests.

## North star: roles and process as data

The end state this architecture is deliberately shaped for: **defining a new agent, or a
new process, should be data, not a code change.**

- Stages/leases are already strings, not enums — a new stage costs no schema change.
- A role definition is (name, system prompt, trigger query, lease kind, output contract).
  Today those live in code per use-case; the target is a `roles` table in config/state
  that the scanner reads, so "add a compliance-review agent between test and merge"
  becomes an entry, not a PR.
- The team can already propose changes to its own process (trend sentinel files
  `[trend]` tickets; agents have shipped orchestrator fixes). With roles-as-data, the
  loop closes: a human says "I want a security-review step", the SA designs the role
  entry, a person approves it through the normal gates, and the process itself has
  changed — no deploy.
- Guardrail that never becomes data: the governance gate. Process definitions are
  Cargo.toml-class artifacts — human-eyes only.

## When to go beyond P1/P2-lite

Any of: median PR-open→review-verdict latency > 10 minutes over a week (metric already
recorded); needed concurrency > 3; a fifth "decouple X from the cycle" patch.
