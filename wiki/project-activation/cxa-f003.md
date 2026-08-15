FOLDER: Project Activation

# Project Activation & Walking Skeleton (CXA-F003)

**Keywords:** onboarding, greenfield, brownfield import, walking skeleton, adopt codebase, project lifecycle, scaffold coxagent.json, docker-compose analysis, git clone import

## Overview

CXA-F003 is how a new project comes online in CoXAgent: it activates a workspace and seeds a walking skeleton — the first minimal feature ("Hello-world service with a /health endpoint that builds and runs") so the team has something concrete to build and verify. There are two activation paths in one ticket: greenfield scaffolds a fresh workspace with no existing code; brownfield adopts an already-existing codebase (locally or by cloning a git URL) without touching the imported source. It is for any operator creating projects from the dashboard or CLI.

## How it works

Both paths live in `onboard.rs` and share one entry shape: they refuse to re-onboard a store that already has tickets, establish the ticket-id alias via `derive_alias(name)`, then write files through an injected `StateStorePort`.

**Greenfield — `onboard::greenfield(store, state_dir, name, alias)`** at crates/app/src/onboard.rs:421 : writes a default `coxagent.json` beside the state dir if absent; writes a blank `project_context.md` template (`context_template`) if absent; then seeds exactly one high-priority Feature ticket titled "Walking skeleton" via `AddTicketUseCase`. It returns an operator message ending in the HUMAN GATE: review `project_context.md` before running `coxagent run`.

**Brownfield — `onboard::brownfield(store, state_dir, name, alias, codebase)`** at crates/app/src/onboard.rs:485 : validates that the path exists; adopts the version already declared by manifests via `detect_codebase_version` (the manifest wins over the newest git tag so releases never regress below what's published); calls `ensure_git_repo` to init + baseline-commit when not yet a repo; runs the comprehension pass (`build_repo_map`, indexing into `.coxagent/REPO_MAP.md`, plus stack detection into governance rules); then runs Docker smarts:

- `analyze_docker(codebase)` parses compose files into `ParsedCompose`, cross-checks them against running containers obtained from `running_host_ports()` (via `docker ps --format {{.Ports}}`) against well-known ports (`KNOWN_SERVICES`: postgres 5432 … vault 8200).
- The result classifies each service as reusable (already running), clash (declared AND running), or missing gap.
- These findings are baked into an auto-drafted comprehension context (`smart_comprehension_context`) and used by `seed_smart_tickets`, which adds follow-up Chores only when needed: "Dockerize for deploy", "Fix compose — remove duplicate infra", "Add Dockerfile for build".

The brownfield config gets governance rules from detected manifests plus an auto-detected engine (Opencode preferred over Claude when discovered) and pre-filled git remote from an existing origin.

The hub/dashboard path routes through this same machinery behind an injected factory:
1. HTTP handler checks auth + space membership first so nothing half-scaffolds.
2. It clones your git URL or validates/adopts your local path.
3. Greenfield creates `<workspace>/<id>/codebase/`; local brownfield uses your path in place; git-url import clones into `<workspace>/<id>/codebase/`.
4. Any confirmed goal is merged onto (never replacing) auto-drafted context at lib.rs.

## Usage

Activate from the CLI:

```bash
# Greenfield — scaffold + walking-skeleton seed
coxagent onboard --name "MyApp" --state-dir ~/CoXAgent/myapp/state

# Brownfield — adopt an existing repo on disk
coxagent onboard --name "MyApp" --existing /path/to/repo --state-dir ~/CoXAgent/myapp/state

# Alias auto-derives from name; override explicitly:
coxagent onboard --name "Cool Chat" --alias CXC ...
```

Activate from the dashboard/hub API:

```http
POST /api/projects
Content-Type: application/json

{ "name": "MyApp", "alias": "MYP", "goal": "Ship X to Y users." }
```

```json
{ "ok": true, "id": "<pid>" }
```

Brownfield variants add one field:

```http
POST /api/projects

{ "name":   "Legacy",  "existing": "/home/u/repos/legacy" }     # local adopt
{ "name":   "Vendored","git_url":  "git@github.com:a/b.git" }   # clone + adopt
```

Every activation prints/stamps files written plus any seeded tickets and reminds you of the human gate on `project_context.md`. Re-running against a store that already holds tickets fails with *"workspace already has tickets; refusing to re-onboard"*.

## Interface

CLI subcommand (`Command::Onboard`, crates/presentation/src/cli.rs):

| Identifier | Kind | Behaviour |
|---|---|---|
| Onboard { name } | CLAP subcommand variant | activates one project |
| Onboard.name     | required long flag String | human-readable project name |
| Onboard.alias    | optional long flag String | short ticket-id alias e.g. CXC; else derived from name |
| Onboard.existing | optional long flag PathBuf | adopt this codebase instead of scaffolding |

HTTP endpoints (crates/presentation/src/server/mod.rs): POST `/api/projects` -> `create_project()` with body `CreateProjectReq` returning JSON `{ ok:true, id }`; GET `/api/projects` -> `list_projects()` returning a JSON array plus broken entries.

Request body fields (`CreateProjectReq`, crates/presentation/src/server/requests.rs): name *(required)*; alias *(optional)*; existing *(optional local path)*; git_url *(optional remote to clone)*; goal *(optional confirmed brief)*; space *(optional admin-gated parent space)*. The factory input type `NewProjectReq` at crates/presentation/src/server/mod.rs carries name / alias / existing / git_url / goal.

Pure helper functions in onboard.rs:
- derive_alias(name) -> String  at crates/application/src/state/mod.rs:1247 : caps-of-name or first-three rule.
- detect_codebase_version(codebase) -> Option&lt;SemVer&gt;
- parse_semver(raw) -> Option&lt;SemVer&gt;
- parse_remote(url) -> Option&lt;(provider, owner/repo)&gt;
- ensure_git_repo(dir) -> Result&lt;String, Box&lt;dyn Error&gt;&gt;

## Configuration

Activation itself adds no new runtime config keys beyond what onboarding writes into each new workspace's default `coxagent.json`:

| Setting | Default on activation |
|---|---|
| engine.default.engine | Opencode if discovered on PATH, else Claude *(brownfield only; greenfield keeps Config::default)* |
| engine.default.model | bizbrain&#47;DeepSeek-V4-Pro when Opencode was detected *(brownfield only)* |
| engine.auto_fallback | false *(brownfield only)* |
| architecture rules | populated from detected stack manifests (brownfield only) |
| deploy.host_port | None until assigned later |

Green/brown decision knobs are compile-time constants rather than settings: `KNOWN_SERVICES` port table drives reuse/clash/gap classification; compose filenames probed include docker-compose.yml/yaml and compose.yml/yaml; stack manifests live in the MANIFESTS list inside `detect_stack()`. Greenfield simply writes `Config::default()` with no engine/governance overrides.

No defaults were introduced by CXA-F003 itself beyond these inherited-on-write values.

## Edge cases and limits

Deliberately NOT done:
- No walking skeleton is seeded on brownfield — an adopted app already exists.
- Brownfield never rewrites or reinitialises your real repo; it only initialises git when the directory has no `.git` at all (baseline commit on adoption).
- An unparseable compose file is treated as empty (no services) rather than failing activation.
- A `docker ps` failure returns an empty running-port set, so nothing is misclassified as reusable/clashing.
- Re-onboarding a store that already has tickets is refused outright.
- Git-URL import refuses non-git schemes (must start with git@, https:// or http://).

How it fails / limits:
- Version adoption refuses non-semver tags rather than guessing wrong; if no manifest or tag parses, the version stays as-is.
- Deletion of an imported project removes only CoXAgent scaffolding (`state/`, `coxagent.json`, a symlink), never the original codebase.

## Code map

- crates/app/src/onboard.rs — greenfield() + brownfield() activation, docker analysis (analyze_docker / running_host_ports / parse_compose / KNOWN_SERVICES), smart context drafting, seed_smart_tickets, build_repo_map, detect_stack, ensure_git_repo, parse_remote, detect_codebase_version / parse_semver. Unit tests are inline in this module.
- crates/app/src/lib.rs — Command::Onboard dispatch (line ~105) and onboard_project() factory used by the hub/dashboard (line ~694); merges any confirmed goal onto context; unique_id for workspace ids.
- crates/presentation/src/cli.rs — Command::Onboard CLAP shape and flags.
- crates/presentation/src/server/mod.rs — GET/POST `/api/projects` routes plus NewProjectReq / ProjectHandle types (lines 716).
- crates/presentation/src/server/projects.rs — list_projects(), create_project() handler with auth + space gating and import-path validation.
- crates/presentation/src/server/requests.rs — CreateProjectReq serde request body.
- crates/application/src/config.rs + config_load.rs — Config::default used to scaffold coxagent.json; host-port healing at load.
- crates/application/src/codegraph.rs — CodeGraph::index consumed by build_repo_map to write REPO_MAP.md.

## Related

CXA-F003 seeds the FEAT walking-skeleton ticket that later `RunCycleUseCase` advances through BA/Sa/dev/TEST. The comprehension output (.coxagent/REPO_MAP.md) feeds selection/search pages. Deployment happens through run_dev/deploy gates after activation. Related lifecycle endpoints: POST `/api/projects/:pid` rename (rename_project_ep) and DELETE `/api/projects/:pid` delete_project_ep in projects.rs. Note there is a build-tree mirror at mergetest/crates/* duplicating some files; treat crates/* as authoritative.
