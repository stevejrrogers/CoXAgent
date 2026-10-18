FOLDER: Onboarding

# Project Activation & Walking Skeleton

**Keywords:** onboarding, activation, greenfield, brownfield, walking skeleton, FEAT-000, Dockerize chore, human gate, coxagent onboard, project_context.md

## Overview

Activation is the step between *adopting* a project and *running an autonomous team on it*. `coxagent onboard` (or the dashboard's "New project" flow) scaffolds a workspace and stops at a **human gate**; activation means closing that gate and handing cycle 1 something real to build and verify. For brand-new (greenfield) projects that thing is the seeded **FEAT-000 "Walking skeleton"** — a hello-world service with `/health` that proves the pipeline before real features. For adopted (brownfield) codebases it is instead a **Dockerize for deploy** chore when no Dockerfile/compose exists — exactly what was seeded for Tony Language (`TL`, ticket `TL-C001`). This page documents both paths end to end.

## How it works

Every entry point — CLI `Command::Onboard`, or the dashboard's `create_project` endpoint — calls one of two pure functions in `crates/app/src/onboard.rs`, each taking a [`StateStorePort`](crates/application/src/ports/outbound/state_store.rs) and returning an operator-facing message:

1. [`greenfield`](crates/app/src/onboard.rs#L421) — brand-new workspace. It refuses when `store.load()` already has tickets ("workspace already has tickets; refusing to re-onboard"), derives or accepts an alias via `derive_alias`, writes `coxagent.json` if absent, drafts `state/project_context.md` from [`context_template`](crates/app/src/onboard.rs#L812), then seeds exactly one feature ticket titled **"Walking skeleton"** (`TicketType::Feature`, description "Hello-world service with a /health endpoint that builds and runs.").
2. [`brownfield`](crates/app/src/onboard.rs#L485) — adopt an existing codebase at `<path>`. It refuses on a non-existent path or non-empty backlog; sets `display_name`, adopts the existing version via [`detect_codebase_version`](crates/app/src/onboard.rs#L985) (the manifest wins over the newest git tag); ensures git via [`ensure_git_repo`](crates/app/src/onboard.rs#L733); indexes into `.coxagent/REPO_MAP.md` via [`build_repo_map`](crates/app/src/onboard.rs#L635); detects stack via [`detect_stack`](crates/app/src/onboard.rs#L653); runs docker analysis via [`analyze_docker`](crates/app/src/onboard.rs#L175); writes an auto-drafted context file via `smart_comprehension_context`; then seeds follow-up chores through [`seed_smart_tickets`](crates/app/src/onboard.rs#L327). When no Dockerfile/compose exists this seeds the **Dockerize for deploy** chore — for TL (a Rust repo with neither), that landed as ticket `TL-C001`.

Both paths stop at the human gate: they return messages telling the operator to review/finish `project_context.md`, then run `coxagent run`. Only after that does activation continue into cycle 1.

Dispatch lives in clap's `Command::Onboard { name, alias, existing }` (crates/presentation/src/cli.rs:48), handled by `run()` at crates/app/src/lib.rs:105 which chooses brownfield vs greenfield based on whether `existing.is_some()`. The dashboard equivalent goes through lib's injected factory -> [`create_project`](crates/presentation/src/server/projects.rs#L66), which resolves spaces/auth first, validates import paths stay under the workspace root or `/tmp`, then invokes greenfield or brownfield depending on whether an import path / git URL was given.

## Usage

Greenfield from CLI:

```
coxagent --state-dir ./state onboard --name "My App"
```

Brownfield adoption of Tony Language (the exact TL case):

```
coxagent --state-dir ./state onboard --name "Tony Language" \
    --alias TL --existing /Users/luton/Projects/TonyLanguage
```

The returned message ends roughly like this:

```
Adopted existing project 'Tony Language' (alias TL) at ... .
Git: initialised repository + baseline commit ...
Comprehension: indexed 336 files...
Running infra: none detected
Wrote: <...>/coxagent.json
       <...>/project_context.md
Seeded: TL-C001 (dockerize)

REVIEW: skim ...(auto-drafted from the code)... and the seeded backlog,
then run the team on `<codebase>`.
```

Then activate:

1. Fill in `/path/to/project_context.md`
   (`What this project is`, `Scope for the team`, plus confirming stack/conventions —
   these sections are auto-drafted but flagged _Fill in_).
2. Run cycles:
   ```
   cd <project dir> && coxagent run        # single-project loop
   ```
   Or serve multiple projects from one hub:
   ```
   coxagent hub --registry ./deploy/data/TL      # lists TL as an active project
   ```

For greenfield there is no running service until cycle 1 picks up FEAT-000; for brownfield like TL only a Dockerize chore is seeded by default — so activate = confirm context + let DEV-BUG turn "not deployable" into something compose can start.

Dashboard create request ([CreateProjectReq](crates/presentation/src/server/requests.rs)):

```json
{
  "name": "Tony Language",
  "alias": "TL",
  "existing": "/Users/luton/Projects/TonyLanguage",
  "goal": "<product goal text>"
}
```

Response: `{ "ok": true, "id": "<project-id>" }`.

## Interface

CLI subcommand flags (`Command::Onboard`) — crates/presentation/src/cli.rs:

| flag | type | default | meaning |
|------|------|---------|---------|
| `--name <str>` | String | required | Human-readable project name |
| `--alias <str>` | Option<String> | derived from name (`derive_alias`) | Short ticket-id prefix e.g. TL |
| `--existing <path>` | Option<PathBuf> | None -> greenfield | Adopt this codebase instead of scaffolding |

Global flag present on all subcommands:
- `--state-dir <path>` — directory holding state files; default `.//state`.

Public onboarding functions used by callers:
- async fn [greenfield]`: StateStorePort + state_dir + name + alias -> Result<String>. Seeds FEAT-000 walking skeleton.
- async fn [brownfield]`: additionally takes codebase path; performs git init / index / docker-analysis internally.
- pub fn parse_remote(url)` -> Option<(provider:String,String)> used during adoption to pre-fill git config from an origin remote.
- derive_alias(name)` derives uppercase ticket prefix when none supplied.

HTTP endpoint behind dashboard onboarding:
- POST `/api/projects?space=<sid>` — payload per CreateProjectReq above (registered at crates/presentation/src/server/mod.rs:722); validates import path stays under workspace root or `/tmp`.

After activation succeeds, these state-store adapters carry tickets onward through RUN/TEST/deploy cycles across process/machine boundaries: [SqlStateStore] and [RestStateStore]. Their transport details are covered by CXA-F003's optimistic-concurrency work — see Related below rather than duplicating them here.

## Configuration

No new configuration flags were added by CXA-F003. What gets written during onboarding comes from three sources:

- Defaults baked into application config ([Config::default]) plus two autodetected engine fields when adopting brownfields with opencode present: engine kind defaults to Opencode and model to DeepSeek-V4-Pro when discovered on PATH.
- Repo intelligence detected live from disk at adopt time: stack rules produced by [`detect_stack`](crates/app/src/onboard.rs#L653) govern architecture conformance afterwards; docker analysis drives which chores [`seed_smart_tickets`](crates/app/src/onboard.rs#L327) creates.
- Version adoption follows [`detect_codebase_version`](crates/app/src/onboard.rs#L985) semantics (manifest first, newest git tag fallback).

Because these autodetections depend on machine state at onboarding time rather than flags, two adoptions of otherwise identical repos can legitimately seed different follow-up work. There are no settings exposed on this ticket itself.

## Edge cases and limits

It deliberately does NOT do several things you might expect:

- Re-onboarding is refused once any tickets exist ("workspace already has tickets"), so activating twice requires deleting/recreating rather than merging backlogs.
- Brownfields never get a walking-skeleton feature automatically — only greenfields do. An adopted repo relies instead on its Dockerize chore plus DEV-BUG addressing baseline build failures first (PLAN 3g Flow B step 5/6).
- Both paths stop short of deploying anything real: the human gate is a deliberate checkpoint before agents make product decisions, per PLAN 3g ("cycle 0", the only mandatory human-in-the-loop).
- The Dockerize chore is only seeded when neither compose nor Dockerfile is found; a repo that already has both seeds nothing (message shows "Seeded: none").

## Code map

crates/app/src/onboard.rs — the two activation entry points [`greenfield`](crates/app/src/onboard.rs#L421) and [`brownfield`](crates/app/src/onboard.rs#L485), plus helpers: `context_template`, `smart_comprehension_context`, `build_repo_map`, `detect_stack`, `analyze_docker` / `parse_compose`, `seed_smart_tickets`, `ensure_git_repo`, `parse_remote`, `detect_codebase_version`.

crates/app/src/lib.rs — CLI dispatch in [run()](crates/app/src/lib.rs#L105): chooses brownfield vs greenfield for Command::Onboard; also the dashboard factory wiring used by hub mode, and [`run_loop`](crates/app/src/lib.rs#L967) which runs cycles once activated.

crates/presentation/src/cli.rs — clap definition of Command::Onboard flags (`--name`, `--alias`, `--existing`) at line 48.

crates/presentation/src/server/projects.rs — dashboard lifecycle endpoint [`create_project`](crates/presentation/src/server/projects.rs#L66) plus list_projects / rename / delete.

crates/presentation/src/server/requests.rs — CreateProjectReq payload (name, alias, existing, git_url, goal, space).

deploy/data/TL/ — the concrete TL workspace: coxagent.json, codebase symlink to /Users/luton/Projects/TonyLanguage, state/{state.json, project_context.md} seeded with TL-C001 (dockerize).

## Related

- docs/CXA-F003.md (same id number) — optimistic concurrency on the state store; distinct from this Product-space page. Both concern activating projects but that one covers store transport after onboarding.
- PLAN.md §3g "Onboarding — cycle 0" (lines ~309–340) — product intent behind these functions: Flow A greenfield FEAT-000 walking skeleton, Flow B brownfield baseline-TEST + Dockerize; enforced human gate.
- CXA-F001 REST-store integration (.claude/handoff-rest-runner.md) — how an activated runner reaches its state over REST via RestStateStore instead of direct Postgres.
- crates/app/tests/hexagonal_gate.rs — guard ensuring onboard's IO stays behind StateStorePort adapters.



