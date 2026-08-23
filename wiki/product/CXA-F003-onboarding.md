FOLDER: Onboarding

# TL Project Activation & Walking Skeleton

**Keywords:** onboarding, activate project, walking skeleton, FEAT-000, greenfield scaffold, brownfield adopt, coxagent onboard, create_project, cycle 0

## Overview

Project Activation is the "cycle 0" path that turns an idea or an existing repo into a runnable CoXAgent workspace: it scaffolds `coxagent.json`, writes a `project_context.md`, and seeds a first "Walking skeleton" ticket so the team has something to build and verify immediately. It is for anyone who must bring up a new project (greenfield) or adopt an existing codebase (brownfield) before the autonomous loop can run. Activation always stops at an explicit human gate before `coxagent run`.

> **Ticket-ID caveat.** CXA-F003 already exists as `docs/CXA-F003.md` — "Optimistic Concurrency Control for Cross-Runner Store Saves" (commit `1b85877`) — which is unrelated. This page documents *Project Activation & Walking Skeleton*, implemented in `crates/app/src/onboard.rs`. Do not conflate them; for revision/CAS store locking read that other doc.

## How it works

Two entry points funnel into [`onboard::greenfield`](crates/app/src/onboard.rs) or [`onboard::brownfield`](crates/app/src/onboard.rs):

1. **CLI** — `coxagent onboard` (`Command::Onboard` in [`cli_main`](crates/app/src/lib.rs)) calls one of the two functions directly depending on whether `--existing <path>` was given (`crates/app/src/lib.rs:113-115`).
2. **Hub/dashboard** — `POST /api/projects` -> [`create_project`](crates/presentation/src/server/projects.rs) -> injected factory -> [`onboard_project`](crates/app/src/lib.rs:692), which allocates `<base>/<id>/state`, builds a store via [`make_store`], optionally clones from a git URL first (`git clone <url> <proj_dir>/codebase`, rejecting URLs not starting with `git@`/`https://`/`http://`), then calls greenfield/brownfield and merges any AI-drafted goal into `project_context.md`.

**Greenfield** ([`greenfield<S: StateStorePort>`](crates/app/src/onboard.rs)) refuses to re-onboard when tickets already exist ("workspace already has tickets; refusing to re-onboard"), persists the ticket-id alias via state's [`derive_alias(name)`], writes fresh defaults from [`Config::default()`] to `<root>/coxagent.json`, writes [`context_template(name)`] to `<state_dir>/project_context.md`, then uses [`AddTicketUseCase::execute(AddTicketInput{...})`] to seed one high-priority small Feature titled **"Walking skeleton"** (description "Hello-world service with a /health endpoint that builds and runs."). It returns an operator-facing message naming every file written plus the seeded ticket id.

**Brownfield** ([`brownfield<S>`](crates/app/src/onboard.rs)) adopts an existing codebase in place: validates the path exists and isn't already onboarded, persists alias + display name + detected version via [`detect_codebase_version(codebase)`] (so adopted repos keep their real published version instead of starting at 0.0.0), ensures git with a baseline commit via [`ensure_git_repo(codebase)`], runs comprehension ([build_repo_map / analyze_docker]) so agents read what they adopt, and seeds gap chores only where missing (e.g. "Add Dockerfile for build") instead of a walking skeleton — because for brownfield an app already exists.

Both stop at the same human gate: review and complete `<state_dir>/project_context.md`.

## Usage

Bring up a brand-new project:

```bash
# from inside where you want workspace state
coxagent onboard --name acme --alias CXC
```

Adopt an existing codebase:

```bash
coxagent onboard --name legacy --existing /path/to/repo --alias LEG
```

From the hub API (`hub mode`, requires auth):

```http
POST /api/projects
Content-Type: application/json

{
  "name": "acme",
  "alias": "CXC",
  "existing": null,
  "git_url": null,
  "goal": "Customer-facing billing portal.",
  "space": "<space-id>"
}
```

Greenfield creates `<workspace_root>/<id>/codebase/`, brownfield adopts/clones your repo there; both register in `/api/projects`. Once spaces exist every project must belong to one (`400 space is required`) unless you are super admin.

## Interface

CLI subcommand (`Command::Onboard`, [cli.rs](crates/presentation/src/cli.rs)):

| flag | type | meaning |
|------|------|---------|
| `--name <string>` | required | Human-readable project name |
| `--alias <string>` | optional | Ticket-id prefix (e.g. CXC); auto-derived from name if omitted |
| `--existing <path>` | optional | Adopt this codebase (brownfield); omit for greenfield |

HTTP endpoint — `.route("/api/projects", get(list_projects).post(create_project))` ([mod.rs](crates/presentation/src/server/mod.rs), handler [projects.rs](crates/presentation/src/server/projects.rs)). Request body ([CreateProjectReq](crates/presentation/src/server/requests.rs)):

| field | type | notes |
|-------|------|-------|
| `name` | string | required |
| `alias?` | string/null | default derived from name |
| `existing?` | string/null | local path under allowed dirs; blocked system dirs -> 403 |
| `git_url?` | string/null | must start with git@ , https:// or http:// ; cloned then adopted like brownfield |
| `goal?` | string/null | merged into project_context.md under "# Goal (from onboarding)" |
| `space?` | string/null | target space; required once spaces exist |

Status codes: success on creation; 400 unknown/missing space or empty name; 403 not admin of space / forbidden import path; NOT_IMPLEMENTED outside hub mode ("onboarding is only available in hub mode"); CONFLICT if id already exists.

Core functions ([onboard.rs](crates/app/src/onboard.rs)):
- [`greenfield(store,&Path,name,&str?,Option<String>) -> Result<String,...>`]
- [`brownfield(store,&Path,name,&str?,Option<String>,&Path) -> Result<String,...>`]
- internal helpers: `detect_codebase_version(codebase)`, `ensure_git_repo(codebase)`, `analyze_docker(path)`, `build_repo_map(path)`.

## Configuration

No activation-specific config flags beyond what activation itself writes:
- A missing `<root>/coxagent.json` gets fresh defaults written from [`Config::default()`]; no probing/healing applies yet.
- Alias defaulting uses state's derive_alias so ticket ids stay stable per project.
- The import-path blocklist applies only through the HTTP path; CLI brownfield checks existence directly without those blocklists.
Both functions take any generic implementing StateStorePort<TicketId = ... as _>, so backend choice does not change activation behaviour.

## Edge cases and limits

It deliberately does NOT:
- Re-onboard: refuses once any ticket exists ("refusing to re-onboard"), protecting against double scaffolding.
- Seed a walking skeleton on brownfield (the app already exists); instead it seeds gap chores like a missing Dockerfile chore when compose exists but no Dockerfile.
- Clean up orphaned directories automatically if registration fails after scaffold.
- Provide interactive role drafting yet — these functions produce deterministic scaffolds; the engine-driven interactive wizard is a separate later capability (PLAN §3g).

## Code map

crates/app/src/onboard.rs — `greenfield` / `brownfield` activation functions, docker/compose analysis (`analyze_docker`, port-clash detection), git-repo bootstrap (`ensure_git_repo`), version detection (`detect_codebase_version`), REPO_MAP build, walking-skeleton seeding.
crates/app/src/lib.rs — `cli_main` `Command::Onboard` dispatch (lines ~113-115); hub factory wiring and `onboard_project(NewProjectReq)` including git-url clone, state-dir layout, goal merge into project_context.md (~692-775); project id derivation via unique_id / derive_alias.
crates/presentation/src/cli.rs — `Onboard { --name, --alias, --existing }` CLI flag definitions.
crates/presentation/src/server/projects.rs — `create_project` HTTP handler for POST /api/projects (space admin checks, import-path blocklist).
crates/presentation/src/server/requests.rs — `CreateProjectReq` request shape.
crates/presentation/src/server/mod.rs — route registration `.route("/api/projects", get(list_projects).post(create_project))`.
crates/application/src/use_cases/add_ticket.rs — `AddTicketUseCase::execute(AddTicketInput)` used to mint the Walking skeleton ticket.

## Related

docs/CXA-F003.md — same ticket id, different feature: optimistic concurrency / revision CAS on the state store. Do not conflate with this page.
PLAN.md §3g "Onboarding" — cycle 0 design: greenfield Flow A, brownfield Flow B (`--existing`), refresh mode; FEAT-000 "walking skeleton" seeding source of truth.
docs/handoff-rest-runner.md (.claude/) — REST-store integration context that activation's store choice rides on.
