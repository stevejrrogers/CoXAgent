# CoXAgent

An autonomous multi-agent software team. A continuous loop of role-agents —
BA → SA → PD → DEV → TEST → DOCS, with code-enforced governance — carries a
ticket from proposal to a running, tested, documented build over a managed
codebase. See [PLAN.md](PLAN.md) for the full design.

The core idea: the LLM proposes and implements, but **state invariants live in
code, not in prompts**. Who may change a ticket's status, when a ticket is
"ready", how spend is metered, what the architecture must be — all enforced by
the type system and pure functions, so the agents cannot corrupt the workflow
even when a model misbehaves.

## Architecture

Clean / hexagonal — the dependency rule is enforced by Cargo, not convention. A
layer only depends on layers below it; a violation fails to compile.

```
crates/
├── domain/          # DDD core: Ticket aggregate, transitions, events — no IO
├── application/     # use cases + ports (inbound/outbound traits), auth boundary
├── infrastructure/  # adapters: state stores, engines, deploy, auth
├── presentation/    # inbound adapters: axum server + embedded SPA, clap CLI
└── app/             # composition root — the one place DI happens; binary `coxagent`
```

Ports mean swaps, not rewrites: the state store is `JsonStateStore` locally and
`SqlStateStore` (Postgres) in a multi-tenant hub — the use cases never change.

## The team

| Role | Does | Enforced by |
|------|------|-------------|
| **BA** | Proposes features to the backlog | — |
| **SA** | Technical design; readies non-UI tickets | `design.technical` gate |
| **PD** | Project design system + per-ticket UX | `design.ux` gate; DoR needs both designs for UI |
| **DEV-BUG / DEV-FEATURE** | Implements one claimed ticket | claim = `System` transition |
| **TEST** | Verifies the running build, files bugs | — |
| **DOCS** | Writes an end-user guide per feature | — |
| **PO / SM** | Priority & rejection / sprint cadence | field-level role permissions |
| **Governance** | Checks the codebase against declared stack rules | drift → bugs |

## Status — v2 (multi-tenant, RBAC, feature-complete)

**Core loop.** Six-agent cycle plus architecture governance. Claim/release,
crash recovery, per-agent error isolation, all orchestrator-owned. `Ticket`
aggregate with guarded mutations; transition table + field permissions as pure
functions. Ids carry a per-project alias (`CXC-F001` / `CXC-B001` / `CXC-C001`).

**Design gates.** SA owns the technical design; PD establishes a project-level
**design system** (palette, typography, components) once UI work appears and
authors per-ticket **UX**. The design system is injected into DEV prompts for UI
tickets — proactive design governance, the analogue of the stack rules.

**Multi-project hub.** `coxagent hub` hosts many projects from one dashboard,
each with its own runner; every API and SSE stream is scoped per project. New
projects are onboarded from the UI (scaffold + seed + register live) and
persisted to the hub registry.

**RBAC.** Argon2id-hashed credentials (a JSON user file storing only hashes),
HttpOnly session cookies, brute-force lockout (5 failures → 15-min lock),
middleware that gates every route: writes require an admin, viewers are
read-only. Bootstrap an admin once via env; runs open when no account is
configured.

**Audit.** An append-only security trail records every authenticated mutation
and sign-in (success and failure) with actor, action, and outcome. Admin-only,
exportable; persisted to Postgres (survives restart) or in memory locally.

**Persistence.** `JsonStateStore` (atomic writes, file lock, rolling backups,
auto-repair) locally; `SqlStateStore` (Postgres, JSONB per project, optimistic
concurrency) for the shared hub. Both pass the same contract test.

**Scrum & FinOps.** Optional sprint mode (commit backlog, roll over, report
velocity). Real token/cost usage metered per role into state; a `budget_usd` cap
auto-pauses the loop.

**Teamwork.** Per-ticket and team-channel **discussion threads**; agents post
standups and ship notes, humans reply — live over SSE.

**Deploy & observability.** After DEV, `DockerComposeDeploy` runs
`docker compose up -d --build` so TEST verifies a running build. Every agent
run's prompt + output is written to `logs/transcripts/`; per-cycle activity
trail with audit export.

**Dashboard** (`serve` / `hub`): a cyan-themed multi-view SPA — Overview (alerts,
deploy health, design system), Team, Board, Sprint (velocity), Activity,
Discussion, Cost, Audit (admin), Settings (engine + model-per-role, workflow) —
over a live SSE feed, controllable runner (Resume/Step/Pause), interactive
tickets, login + role-aware UI.

75 tests; clippy pedantic + `-D warnings`; CI + tagged release binaries
(macOS arm64/x64, linux).

## Quickstart

### Single project (local)

```sh
coxagent --state-dir <ws>/state onboard --name "MyApp" --alias APP
coxagent --state-dir <ws>/state serve --work-dir <ws>/codebase   # → localhost:4000, Resume
coxagent --state-dir <ws>/state check --work-dir <ws>/codebase   # governance only (CI)
scripts/install-launchd.sh <ws>                                  # run 24/7 (macOS)
```

### Multi-project hub

A registry is a JSON array of `{ "id", "path" }`; each `path` is a workspace with
`state/` and `codebase/`. New projects can also be created from the dashboard.

```sh
echo '[{"id":"myapp","path":"/srv/myapp"}]' > registry.json
coxagent hub --registry registry.json --port 4000
```

### Enterprise: RBAC + Postgres

```sh
# Bootstrap the admin once (seeds a hashed auth.json beside the registry).
export COXAGENT_ADMIN_USER=root
export COXAGENT_ADMIN_PASSWORD='••••••••'
# Shared multi-tenant persistence (else the local JSON store is used).
export COXAGENT_DB_DSN='postgres://user:pass@host:5432/coxagent'
coxagent hub --registry registry.json --port 4000
```

`auth.json` holds only Argon2 hashes and is git-ignored — never commit it.

## Engines

`claude`, `opencode`, `scripted` (offline, deterministic — writes a real runnable
app + Docker files), and `mock`, all behind one port with metering and transcript
decorators. Engine and model are configurable per role in `coxagent.json` or from
the dashboard's Settings screen.

## Develop

```sh
cargo test && cargo clippy --all-targets && cargo fmt --all
# Postgres adapter contract (needs a database):
COXAGENT_TEST_PG_DSN='postgres://…' cargo test -p coxagent-infrastructure --test sql_store_contract
```
