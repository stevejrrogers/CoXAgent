# CoXAgent

An autonomous multi-agent software team. A continuous loop of role-agents —
BA → SA → PD → DEV → TEST → DOCS, with code-enforced governance — carries a
ticket from proposal to a running, tested, documented build over a managed
codebase. See [ARCHITECTURE.md](ARCHITECTURE.md) for the current architecture,
[DEPLOYMENT.md](DEPLOYMENT.md) for the three deploy shapes (macOS app,
docker-compose, Kubernetes/Helm), and [PLAN.md](PLAN.md) for the original design.

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
├── contracts/       # versioned wire types between services (event bus, jobs)
├── infrastructure/  # adapters: state stores, engines, deploy, auth
├── presentation/    # inbound adapters: axum REST + WS + MCP, embedded SPA
└── app/             # composition root; binaries: coxagent, cox-all,
                     #   cox-gateway, cox-realtime, cox-knowledge
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

### How the team stays smart, cheap and safe

- **Context, not exploration**: agents get a repo map + code-graph MCP tools,
  a relevance-ranked team memory, per-ticket work journals on retries, and
  cross-project hub lessons — no cold re-reading every run.
- **Escalation ladder**: a failed ticket retries on a stronger model
  (`engine.escalation`; opencode auto-prefers your custom providers), and the
  repair/PR-fix passes RESUME the same conversation instead of starting over.
- **Quality gates**: TDD (TEST writes failing tests from acceptance criteria
  before DEV codes), an SA critic pass on large designs, PD visual QA on real
  screenshots, and context-appropriate **DoD evidence posted on the ticket
  thread** — UI ⇒ screenshot, API ⇒ live request/response — required before
  Verified.
- **Self-tuning**: the loop reads its own evals daily and pulls its own
  brakes (churn hot → bugs first; backlog fat → BA pauses), announced by SM.
- **FinOps & safety**: budget caps (total/daily), a cost-approval gate for
  expensive tickets, host-wide heavy-op gate + `nice`, per-workspace orphan
  cleanup, docker resource clamps, and an opt-in write-sandbox (macOS
  Seatbelt) for agent CLIs.
- **Steer it live**: comment on an in-progress ticket and the agent's next
  run treats it as instructions.

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

**RBAC.** Argon2id-hashed credentials, HttpOnly session cookies, brute-force
lockout (5 failures → 15-min lock), middleware that gates every route: writes
require an admin, viewers are read-only. Bootstrap an admin from env
(authoritative — it always wins over a stale file); runs open when no account is
configured. Accounts, tokens, and 2FA secrets live in a local `auth.json` (only
hashes) or, when `COXAGENT_AUTH_DSN` is set, in **Postgres**; **sessions persist
in Redis** (native-TTL keys) so a hub relaunch never signs anyone out.
**API tokens** for automation: admins mint scoped service-account tokens (stored
as SHA-256 hashes, shown once) used via `Authorization: Bearer`.

**Audit.** An append-only security trail records every authenticated mutation
and sign-in (success and failure) with actor, action, and outcome. Admin-only,
exportable; persisted to Postgres (survives restart, with an optional retention
window) or in memory locally.

**Access management.** Admins manage teammates and automation from the
dashboard: add/remove user accounts (the last admin is protected) and mint or
revoke API tokens — no file editing.

**Policy.** Opt-in governance in config: a model allowlist (the loop refuses to
run on a disallowed model), a per-day spend cap (pauses independently of the
lifetime cap), and forbidden-path evaluation — human gates as configuration.

**Roadmap.** A self-generating Now / Next / Later / Shipped timeline rendered
from tickets + sprint + dependencies + priority — never hand-maintained.

**Onboarding.** Greenfield (`onboard`) scaffolds a new workspace; brownfield
(`onboard --existing <path>`) adopts a codebase — git-inits it and seeds
follow-up work.

**Persistence.** `JsonStateStore` (atomic writes, file lock, rolling backups,
auto-repair) locally; `SqlStateStore` (Postgres, JSONB per project, optimistic
concurrency) for the shared hub. Both pass the same contract test. Where each
kind of data lives when the infra is configured: **Postgres** — project state,
accounts/tokens, audit log, system chat; **Redis** — sessions + all ephemeral
coordination (leases, worker registry, operator locks, desired-run flags);
**MongoDB** — the living documentation store; **MinIO/S3** — chat & ticket media.
Only two bootstrap files stay machine-local by design: `coordination.json` (the
DSNs themselves) and `registry.json` (project id → local codebase path).

**Distributed & multi-operator (SaaS).** Many operators (`account@host`) work one
project in parallel, coordinating through Postgres + Redis: an atomic
`claim_ticket`, per-stage leases, and leader election mean no two ever duplicate
work; shared-state writes use optimistic concurrency **with retry** so neither
loses an update; each operator builds in its own git worktree; a **single-instance
lock** refuses a second process for the same `account@host`. **Start/stop is
per-user** — each controls only their own operator (a persisted desired-run flag
the operator honours, so Stop reaches a worker on another machine without killing
processes; admins can control any). The desktop app runs an embedded hub by
default, or, with `COXAGENT_HUB_URL` set, becomes a **thin client** onto a central
hub and spawns its own operator (idle until started). Token spend is metered
**per operator** for per-user FinOps, and each operator's **live log** is viewable
separately in the dashboard.

**Scrum & FinOps.** Optional sprint mode (commit backlog, roll over, report
velocity). Real token/cost usage metered per role **and per operator** into state;
a `budget_usd` cap auto-pauses the loop. **Token-optimized ceremonies**: standup /
planning / grooming / discussion each run in a *single* engine call (not one per
role turn), fire only when there's real activity, and route to a cheap model
(`per_role`, e.g. haiku for `sm`/`docs`) — an order-of-magnitude cut with no loss
of the human-team feel.

**Teamwork.** Per-ticket and team-channel **discussion threads**; agents post
standups and ship notes, humans reply — live over SSE.

**Deploy & observability.** After DEV, `DockerComposeDeploy` runs
`docker compose up -d --build` so TEST verifies a running build. Every agent
run's prompt + output is written to `logs/transcripts/`; per-cycle activity
trail with audit export.

**Dashboard** (`serve` / `hub`): a cyan-themed multi-view SPA — Overview (alerts,
deploy health, design system), Team, Board, Sprint (velocity), Activity,
Roadmap, Discussion, Cost, Audit (admin), Settings (engine + model-per-role,
workflow) — over a live SSE feed, controllable runner (Resume/Step/Pause),
interactive tickets, login + role-aware UI.

150 tests; clippy pedantic + `-D warnings`; CI + tagged release binaries
(macOS arm64/x64, linux).

## Quickstart

### Desktop apps (no browser, no manual server)

- **CoXAgent.app / CoXAgent.exe / CoXAgent (Linux)** — native window that boots
  the bundled hub and hosts the dashboard in the system webview
  (`desktop/coxagent-desktop`, tao + wry; macOS ships the Swift shell). Reuses an
  already-running hub instead of double-spawning; hub output goes to
  `~/CoXAgent/logs/hub.log`; port override via `COXAGENT_DESKTOP_PORT`.
- **CoXAgent Companion** (macOS menu bar) — hub health at a glance + quick
  actions (open dashboard / launch app / open log). Build:
  `scripts/build-companion.sh`. All three: `scripts/build-desktop.sh` → `dist/`.

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

### Self-host with Docker

Run the hub + dashboard in a container, backed by Postgres (state, audit,
retention). The server binds `0.0.0.0` in-container (`COXAGENT_HOST`) so the
published port is reachable.

```sh
COXAGENT_ADMIN_PASSWORD=yourpw docker compose up -d --build
# → http://localhost:8101, log in as root / yourpw
```

Real LLM agents need a CLI (`claude`/`opencode`) on PATH inside the container;
the base image ships the offline `scripted`/`mock` engines — install a CLI in a
derived image to run real agents in the container.

### Enterprise: distributed hub (Postgres + Redis + Mongo + MinIO)

```sh
export COXAGENT_ADMIN_USER=root COXAGENT_ADMIN_PASSWORD='••••••••'
export COXAGENT_DB_DSN='postgres://user:pass@host:5432/coxagent'    # state, chat, audit
export COXAGENT_AUTH_DSN="$COXAGENT_DB_DSN"                         # accounts in Postgres
export COXAGENT_REDIS_URL='redis://host:6379'                      # sessions + coordination
export COXAGENT_MONGO_URL='mongodb://host:27017'                   # docs (optional)
export COXAGENT_S3_ENDPOINT='http://host:9000'                     # media (optional)
coxagent hub --registry registry.json --port 4000
```

The desktop app reads these from a `coordination.json` next to the registry, so a
Finder launch is distributed without env. Extra operators join the same project
by running `coxagent … run` (or a thin-client app, `COXAGENT_HUB_URL=…`) with a
distinct `COXAGENT_OPERATOR`. `auth.json` (file mode) holds only Argon2 hashes and
is git-ignored — never commit it.

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
