FOLDER: Project Activation
# Project Activation & Walking Skeleton

**Keywords:** onboard, onboarding, greenfield, brownfield, walking skeleton, project_context.md, coxagent.json, REPO_MAP.md, human gate

## Overview

Project activation brings a workspace under CoXAgent management before the autonomous loop starts. It either scaffolds a brand-new project (greenfield) or adopts an existing codebase in place (brownfield), writing the runtime artifacts the team depends on — `state`, `coxagent.json`, `project_context.md` — and seeding a starting backlog. Both paths stop at a human gate: nothing runs until the operator reviews and completes the drafted context. Greenfield additionally seeds the FEAT-000 "Walking skeleton" feature ticket so cycle one has one small concrete thing to build and verify.

## How it works

The CLI and hub paths converge on two scaffolders in [`crates/app/src/onboard.rs`](crates/app/src/onboard.rs).

**Greenfield** ([`greenfield()`](crates/app/src/onboard.rs)) is for projects that do not exist yet:
- Refuses to re-onboard if `store.load()` already has tickets ("workspace already has tickets; refusing to re-onboard").
- Derives or accepts the ticket-id alias via [`derive_alias()`](crates/application/src/state/mod.rs): up to 4 leading uppercase letters of the name (`CoXChat` -> `CXC`), else first three alphanumerics uppercased.
- Writes `coxagent.json` from [`Config::default()`](crates/application/src/config.rs) if absent.
- Writes draft `project_context.md` from [`context_template()`](crates/app/src/onboard.rs).
- Seeds exactly one Feature ticket titled **"Walking skeleton"** ("Hello-world service with a /health endpoint that builds and runs") via [`AddTicketUseCase::execute(AddTicketInput{..})`](crates/application/src/use_cases/add_ticket.rs).

**Brownfield** ([`brownfield()`](crates/app/src/onboard.rs)) adopts an existing codebase at `codebase`:
- Adopts its declared version via [`detect_codebase_version()`] — manifest wins over newest git tag so release bumps don't collide.
- Ensures git via [`ensure_git_repo()`]: init + baseline commit when not already a repo; detects an existing origin.
- Runs comprehension: indexes into `.coxagent/REPO_MAP.md` via [`build_repo_map()`] -> [`CodeGraph::index`]; detects stack via [`detect_stack()`]; analyzes docker via [`analyze_docker()`].
- Drafts real context from what it learned ([`smart_comprehension_context()`]) instead of a blank template.
- Seeds governance rules plus follow-up chores via [`seed_smart_tickets()`] based on docker analysis.

**Hub/dashboard path** ([`onboard_project()`](crates/app/src/lib.rs)) wraps either scaffolder under a registry directory: validates space/auth up front (no half-registered orphan), optionally git-clones from `git_url`, merges any goal into context, assigns host port via `assign_host_port(base, registry_path, &proj_dir)`, appends to registry (`append_registry`, atomic rename), then builds runner handle with `build_project`.

Every path ends by printing where files were written and telling the operator to complete `project_context.md`.

## Usage

### Greenfield (CLI)

```sh
coxagent --state-dir ./myapp/state onboard --name "My App"
```

Output:

```
Onboarded project 'My App' (alias MA).
Wrote: <root>/coxagent.json
       <root>/state/project_context.md
Seeded: FEAT-000 (walking skeleton)

HUMAN GATE: review and complete state/project_context.md before running `coxagent run`.
```

With an explicit alias:

```sh
coxagent onboard --name "CoX Chat Service" --alias CXC
```

### Brownfield adoption (CLI)

```sh
coxagent onboard --name "Legacy Tool" --existing /path/to/repo
```

Output:

```
Adopted existing project 'Legacy Tool' ... indexed N files ...
Git: initialised repository + baseline commit ...
REVIEW: skim state/project_context.md ... then run the team.
```

### Dashboard / hub API

Create a project by POSTing to the project collection route with JSON body:

```
POST /api/projects
Content-Type: application/json
```

```json
{
  "name": "Billing",
  "alias": "BIL",
  "space": "<space-id>",
  "existing": null,
  "git_url": null,
  "goal": "(optional AI-drafted product brief)"
}
```

Returns HTTP 200 with the new project's id; it appears immediately in a subsequent `GET /api/projects`.

After activation you start cycles normally:

```sh
cd <workspace>/codebase && coxagent run
```

## Interface

### CLI command (`Command::Onboard`, crates/presentation/src/cli.rs)

| Flag | Type | Default | Meaning |
|------|------|---------|---------|
| `--name <N>` | String | required | Human-readable project name |
| `--alias <A>` | Option<String> | derived from name | Short ticket-id alias (`CXC`) |
| `--existing <PATH>` | Option<PathBuf> | none -> greenfield | Adopt this codebase instead of scaffolding |

Global flag on every subcommand: `--state-dir <DIR>`, default `./state`. Dispatch is in crates/app/src/lib.rs (`Command::Onboard { name, alias, existing }`), calling brownfield or greenfield depending on whether an existing path was given.

### HTTP endpoint (hub mode only; crates/presentation/src/server/projects.rs)

- Route `POST /api/projects` (collection route; `GET /api/projects` lists). Registered in crates/presentation/src/server/mod.rs:716.
- HTTP **501 NOT_IMPLEMENTED** when no factory is injected (`app.factory == None`, i.e. not hub mode).
- Space checks fail fast before scaffolding: unknown space -> HTTP **400**; non-admin of target space -> HTTP **403**. Once any space exists every new project must declare one (**400** otherwise).
- Brownfield import path is canonicalized and blocked if under system dirs (`/etc`, `/root`, `/usr/*`, `/bin`, etc.) -> HTTP **403**.

The server resolves auth/spaces then calls the injected factory which routes through [`onboard_project()`](crates/app/src/lib.rs). Related lifecycle endpoints (list/create/delete) sit beside it.

## Configuration

Activation itself needs no manual configuration at first scaffold — defaults suffice; you edit generated artifacts afterwards. Notable behaviours:

- **State store backend**: chosen by how activation is invoked — CLI uses whatever backend matches its pid/store wiring; hub uses per-project Postgres-backed stores. No per-activation flag changes this today.
- **Engine defaults** (brownfield writes these into fresh config): if opencode is detected on PATH it becomes default engine with model set accordingly, otherwise Claude; `engine.auto_fallback` set off.
- **Version adoption** (brownfield): takes an existing codebase's declared version over its newest git tag (manifest wins).

There are no activation-specific flags beyond `--name`, `--alias`, `--existing`.

## Edge cases and limits

It deliberately does NOT cover:

- Re-onboarding fails closed ("workspace already has tickets") when tickets exist; you cannot re-run activation over an active backlog.
- A codebase with no parseable manifest version and no semver tag keeps its version unset rather than guessing wrong (`parse_semver` refuses names like "release-summer" or dates).
- Import path sandboxing rejects system-directory paths regardless of intent.
- Greenfields seed exactly one walking-skeleton ticket — everything else comes from later BA cycles against your completed context.
- Docker analysis reflects host state at onboarding time only; running containers may change afterwards.

## Code map

- crates/app/src/onboard.rs — greenfield/brownfield scaffolders, docker analysis (`analyze_docker`), stack detect (`detect_stack`), comprehension context drafting, seed_smart_tickets, detect_codebase_version.
- crates/app/src/lib.rs — CLI dispatch for Onboard; hub factory + onboard_project (git clone, port assign, registry append).
- crates/presentation/src/cli.rs — Command::Onboard flag shape.
- crates/presentation/src/server/projects.rs — create/list/delete project endpoints and auth/space checks.
- crates/presentation/src/server/mod.rs — route registration / injected ProjectFactory wiring for hub mode.
- crates/app/src/config_load.rs — load_config used after onboarding to drive later runs.
- crates/app/src/host_port.rs — assign_host_port: collision-free host port selection at onboard time.
- crates/application/src/config.rs — Config::default() / DeployConfig written to coxagent.json.
- crates/application/src/state/mod.rs — derive_alias() and ProjectState alias handling.
- crates/application/src/use_cases/add_ticket.rs — AddTicketUseCase / AddTicketInput used to seed the walking skeleton.

## Related

Ongoing-work note `.claude/handoff-rest-runner.md` covers adjacent state-store/REST/auth work. Docs for the cycle runner that consumes a completed context live under the run_dev use-cases (cycle loop), while REPO_MAP generation shares code with onboarding's comprehension pass.



