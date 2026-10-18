# Tony Language — project context

_Auto-drafted on adoption; refined during the walking-skeleton activation cycle (CXA-F003)._

## Stack (detected)
- **root** — Rust workspace (`Cargo.toml`), one crate per compiler stage
  (`tl-ast` → `tl-lexer`/`tl-parser` → `tl-hir` → `tl-typecheck` → `tl-mir` →
  `tl-interpreter`) plus `tl-driver` as the CLI/toolchain binary.

## Deploy
Dockerized for deploy: `Dockerfile.builder` builds the `tl-driver` binary;
`docker-compose.yml` runs it as a service serving `/health`. Walking skeleton
published on host port **8101** (this project's single reserved host port).

## What this project is
Tony Language ("TL") is a general-purpose systems programming language that
treats distributed data as a first-class citizen. It targets developers building
scalable backends, distributed services, and data-intensive applications who want
memory/thread safety with an easier learning curve than Rust and stricter typing
than Go. Core value: databases, SQL, streams, actors and CRDTs are native language
constructs rather than bolted-on libraries.

### Target user
Language users writing TL application/service code; contributors extending the
compiler; operators deploying TL services in containers.

### Must-have (MVP scope for this team)
1. A working end-to-end compile+run pipeline for simple TL programs.
2. Deterministic CLI stages (`lex`, `parse`, `typecheck`, `run`) with timings.
3. A deployable container running a TL program that answers `/health`.

### Success criteria (for this activation cycle)
- Walking skeleton runs clean end-to-end: build → tests green → compose up →
  `/health` returns 200 + healthy JSON within ~5s of launch.
- Every AC of ticket CXA-F003 passes; walking skeleton tracked as FEAT-TL-C002.

## Scope for the team (next priorities)
1. Land and verify the walking skeleton; burn down any baseline bugs surfaced by
   TEST (e.g., failing existing unit tests).
2. Grow from TL-C001 'Dockerize for deploy' once verified working.
3. Add real features from TL backlog seeded by BA after baseline stabilizes.
4. Do not rebind host port 8101 or other published host ports without approval;
   reuse shared infra rather than deploying duplicates.

## Conventions to respect
- Workspace-level lint gates: rustc warnings = deny, clippy pedantic warn,
  unsafe_code forbidden; keep new code warning-free (`cargo build --workspace`,
  then individual crate tests).
- One cohesive unit per file; additive changes preferred over rewrites given an
  active parallel workspace — never mass-move files unprompted.
- `.tn` examples live under `examples/`, tests under each crate's own source or
  integration dir (`crates/<crate>/tests/*.rs`) driving the real binary via
  `CARGO_BIN_EXE_tl-driver`.
- Health probes live in tl-runtime metrics server (`GET /health|/healthz|/readyz|/metrics`);
  new behaviour ships with its test.
