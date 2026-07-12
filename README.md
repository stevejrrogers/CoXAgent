# CoXAgent

Autonomous multi-agent software team that runs a continuous BA → SA → DEV → TEST → DOCS
loop over a managed codebase. See [PLAN.md](PLAN.md) for the full design (roles,
lifecycle, sprint/scrum, teamwork, hub & workers, enterprise track).

## Architecture

Clean / hexagonal architecture — the dependency rule is enforced by Cargo, not by
convention. A layer can only depend on the layers below it; a violation fails to compile.

```
crates/
├── domain/          # DDD core: Ticket aggregate, transitions, events — no IO, no deps
├── application/     # use cases + ports (inbound/outbound traits)
├── infrastructure/  # outbound adapters (JsonStateStore; engines/deploy/git later)
├── presentation/    # inbound adapters (report now; axum/CLI later)
└── app/             # composition root — the one place DI happens; binary `coxagent`
```

## Status — v1 (local single-user, feature-complete)

- **Six-agent cycle**: BA → SA (design gate) → DEV-BUG → DEV-FEATURE → TEST → DOCS,
  plus a code-enforced **architecture governance** step. Claim/release, crash
  recovery, and per-agent error isolation are all orchestrator-owned.
- **Domain**: `Ticket` aggregate (guarded mutations), transition table + field-level
  role permissions as pure functions. Ids carry a per-project alias (`CXC-F001`).
- **State**: `JsonStateStore` with atomic writes, file lock, rolling backups,
  auto-repair from backup, `schema_version` guard. Contract-tested for substitutability.
- **Engines** behind one port: `opencode`, `claude`, `scripted` (offline), `mock`.
- **Dashboard**: `coxagent serve` hosts a controllable runner (Resume/Step/Pause),
  live SSE, KPIs, kanban, changelog, metrics API.
- **Governance**: declared stack rules are injected into agent prompts (proactive)
  and checked against the codebase each cycle (reactive) — drift becomes bugs.
- 58 tests; clippy pedantic + `-D warnings`; CI.

Multi-tenant hub, RBAC/SSO, and the enterprise tier are the documented v2 roadmap
(see PLAN.md), not part of this v1.

## Use

```sh
coxagent --state-dir <ws>/state onboard --name "MyApp" --alias APP
coxagent --state-dir <ws>/state serve --work-dir <ws>/codebase   # → localhost:4000, Resume
coxagent --state-dir <ws>/state check --work-dir <ws>/codebase   # governance only (CI)
scripts/install-launchd.sh <ws>                                  # run 24/7 (macOS)
```

## Develop

```sh
cargo test && cargo clippy --all-targets && cargo fmt --all
```
