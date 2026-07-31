# CoXAgent — Architecture

> Status: modular monolith today, four-service target approved. This document is
> the single source of truth for the shape of the system; update it in the same
> PR as any structural change.

## Layers (hexagonal — unchanged across all deploy shapes)

```
crates/
├── domain/          # Entities & invariants (tickets, roles, transitions). No IO.
├── application/     # Use-cases + ports. ALL orchestration law lives here
│                    # (cycle, merge queue, recovery mode, memory hygiene…).
├── contracts/       # Versioned wire types between services (BusEnvelope, JobSpec).
├── infrastructure/  # Adapters: Postgres/Redis/MinIO/Mongo, gh/glab, docker, pty,
│                    # engines (claude/opencode), tree-sitter codegraph.
├── presentation/    # HTTP/WS/MCP surface + embedded SPA (web/index.html).
├── app/             # Composition root, CLI (`coxagent`) + self-host `cox-all`.
└── services/        # Production binaries, one thin crate per plane:
                     #   gateway/ · runner/ · realtime/ · knowledge/
```

Rules that keep this healthy:

- **Dependencies point inward only.** `application` never imports `infrastructure`.
- **Process law is code, not prompts** — WIP limits, recovery mode, conflict
  verification, budget caps are enforced in `application`, and summarized for
  agents in `prompts::PROCESS_INVARIANTS` (update both together).
- **Every external system sits behind a port.** Swapping Postgres↔file,
  gh↔glab, claude↔opencode is an adapter change, never a use-case change.

## Runtime today (two roles, one binary)

- **hub** — serves the dashboard + API + WS + MCP-to-be, owns hub-level
  watchdogs (space budgets, nightly backups, Redis event bus bridge).
- **operator/runner** — executes agent cycles (BA→SA→DEV→TEST), git, docker
  deploys. Many operators coordinate through Postgres (leader election,
  per-ticket claims with leases) — never through shared memory.

## Target: four services (approved)

| Service | Role | Scaling |
|---|---|---|
| cox-gateway | Control plane: REST + **MCP** + authz + audit + policy | Stateless, N replicas |
| cox-runner | Execution plane: agents, git, docker, PTY terminals | Per-tenant containers, resource-capped |
| cox-realtime | Long-lived WS: chat, presence, docs collab, terminal bridge | Scales on connection count |
| cox-knowledge | Batch: codegraph, wiki, memory hygiene, digests | Single instance, restart-safe |

Key decisions:

- **MCP is a gateway transport, not a fifth service** — same use-cases, same
  authz, same audit as REST.
- **Self-host still ships one binary** (`cox-all` composes all four in-process);
  SaaS deploys them separately. Same code, two deploy shapes.
- Gateway↔runner traffic rides Postgres (claims + leases) until real load
  justifies a queue (NATS slot reserved in `contracts`).
- Cross-instance realtime rides Redis pub/sub (`cox:events`, `BusEnvelope`).

## Data stores

| Store | Holds | Notes |
|---|---|---|
| Postgres | Project state, auth, audit, job claims | System of record; app_kv JSON docs (workspace/spaces/chat) |
| Redis | Sessions, leader/lease coordination, event bus | Ephemeral by design |
| MinIO (S3) | Chat/ticket files, agent logs | |
| MongoDB | Wiki/docs pages | Optional; falls back to state.json |
| `<hub>/backups/` | Nightly JSON snapshots of hub docs | 14-day retention |

## Security model

- Roles: `super` (hub-wide) → `admin` → leads → members → `viewer` (read-only).
  Every privileged rule is enforced **server-side** and audited; UI hiding is
  cosmetics only.
- DMs are participants-only — no admin override, by test.
- Terminal/PTY: any writing member, every session audited. On hardened control
  planes `COXAGENT_NO_INLINE_EXEC=1` refuses shells outright (the Helm hub sets
  it) — terminals belong to the execution plane.
- Human-ordered execution (force-merge) is **queued** (`state.jobs`) and picked
  up by a live runner within ~15s; the hub only executes inline when no runner
  is registered (solo-desktop mode).
- Session cookies: HttpOnly + SameSite=Strict, `Secure` added behind TLS.
- Secrets: env vars first; `coordination.json` supports `${VAR}` placeholders
  and is clamped to mode 600.

## Agent process law (the parts that keep production sane)

- PR queue: WIP limit gates new branches; ≥2× limit trips **recovery mode**
  (merge-only cycles, resolver-PRs auto-closed, BA/TEST paused).
- Conflicts: resolved on the original branch, then **verified** (no committed
  markers + forge reports mergeable) before anything may merge.
- Deploys: compose-only with deterministic project names (`cox-<parent>-<dir>`),
  self-healing port squatters (compose project → raw container), red deploys
  retry next cycle without new code, and an hourly janitor removes dead
  `cox-*` projects + dangling images (never the `cox-infra` backing group).
- Engine memory: daily hygiene judges `~/.claude` project memory against
  `PROCESS_INVARIANTS`; durable lessons are promoted into `CLAUDE.md`.

## Module layout inside the big crates (July–August 2026 refactor)

The two conflict magnets — `use_cases/cycle.rs` (7,000 lines) and
`presentation/src/server.rs` (9,600) — were split into directories. The rule
behind every cut: **one file per responsibility a person can name**. If you
cannot say what the new file is FOR, the split made two problems out of one.

```
application/src/use_cases/
├── cycle/                  # the orchestrator, split by job
│   ├── mod.rs              #   run_cycle sequencing + report (~1.5k)
│   ├── ceremonies.rs       #   sprint boundary, standup, grooming, digest, self-tune
│   ├── forge.rs            #   PR review/merge/rebase, verify-merged-result
│   ├── ops.rs              #   deploy, health gate, rollback to last known-good
│   ├── qa_evidence.rs      #   what a test result must SHOW (screenshots, req/resp)
│   ├── escalation.rs       #   agent questions, escalating parked tickets
│   ├── scrum.rs            #   scrum topic, next-feature clarification, activity record
│   ├── wiring.rs           #   one builder per agent role (BA/SA/PD/DEV/TEST/DOCS…)
│   └── cycle_tests.rs      #   the full-cycle integration tests
├── run_dev/                # DEV pass: mod, briefing, gates (pure), failures
└── merge_policy.rs         # pure decisions: who unsticks a ticket, competing
                            # PRs, what is too big to auto-merge

presentation/src/server/    # the HTTP router, split by surface
├── mod.rs                  #   router, middleware, core (~2.1k)
├── chat.rs                 #   channels, DMs, reactions, uploads, delivery
├── auth.rs                 #   sign-in, sessions cookie flow, avatar upload
├── people.rs               #   users, tokens, members, profiles, analytics
├── transcripts.rs          #   live agent-log tail, transcript downloads
├── docs.rs                 #   Wiki pages, folders, AI edits, docs-ws
├── forge.rs                #   PRs: review, merge, preview, git settings
├── work.rs                 #   tickets, sprints, runner controls
├── meetings.rs             #   booking, joining, the ring, watchdog
└── …assets/engines/manage/projects/status/comments/channels/background/realtime
```

**IO discipline is now total**: `crates/app/tests/hexagonal_gate.rs` forbids
`std::process`/`std::fs` in ALL of the application layer's production code —
its grandfather list is empty and may only stay so. Every effect goes through
`ports/outbound/` (`GitPort`, `WorkspaceFilesPort`, `ScreenshotPort`,
`ProcessJanitorPort`, …); decisions are pure functions of a snapshot the
adapter takes once.

Conventions for these split modules:

- Children use `pub(super)` and `use super::*` — they are ONE logical module
  split for merge-conflict surface, not an API boundary. The wildcard import
  is allowed there deliberately (see the header note in each file).
- Moving a method is mechanical: same signature, doc comment travels with it,
  no logic edits in the same commit as the move.
- When cutting, take the item's doc comments and `#[derive]` attributes WITH
  it — an orphaned attribute above the seam is the classic split bug.
- The COX-B009 deploy-gate guard (`crates/app/tests/health_gate.rs`) scans all
  of `src/`; when a split introduces a new visibility form, the guard must
  keep recognising `fn` headers — it caught `pub(super)` being invisible once.

Remaining oversized (split when work takes you into them):
`presentation/src/web/index.html` (~7.7k — needs per-view JS/CSS split with a
UI smoke test), `cycle/forge.rs` (1.46k), `server/mod.rs` router body.
