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

## Status — M0 (foundation)

- Domain model: `Ticket` aggregate root with private fields, parse-don't-validate
  construction, and guarded mutations.
- Transition table + field-level role permissions as pure functions (enforce-by-code).
- `StateStorePort` trait + `JsonStateStore`: atomic writes (temp + rename), advisory
  file lock, validation-before-save, rolling backups, `schema_version` guard.
- Contract test suite proving Liskov substitutability for any store adapter.
- CI: `cargo fmt --check`, `clippy` pedantic with `-D warnings`, tests.

## Develop

```sh
cargo test              # domain unit + use-case + store contract tests
cargo clippy --all-targets
cargo fmt --all
cargo run -p coxagent-app -- ./state   # seed walking skeleton + print state report
```
