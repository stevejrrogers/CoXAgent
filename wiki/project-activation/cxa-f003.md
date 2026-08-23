FOLDER: Project Activation

# Project Activation & Walking Skeleton (CXA-F003)

**Keywords:** onboarding, greenfield scaffold, brownfield import, walking skeleton FEAT-000, adopt codebase, docker-compose analysis, git clone import, derive_alias

## Overview

CXA-F003 is how a new project comes online in CoXAgent: it activates a workspace and seeds a walking skeleton - the first minimal Feature ticket ("Hello-world service with a /health endpoint that builds and runs") so there is something concrete for BA/Sa/dev/TEST to advance through RunCycleUseCase. It supports two activation paths in one ticket: greenfield scaffolds a fresh workspace when there is no existing code; brownfield adopts an already-existing codebase - locally or by cloning a git URL - without touching its source. It is for any operator creating projects from either the CLI (coxagent onboard) or the hub/dashboard (POST /api/projects).

## How it works

Both paths live in crates/app/src/onboard.rs over StateStorePort. They share one entry shape: they refuse to re-onboard any store that already holds tickets ("workspace already has tickets; refusing to re-onboard"), establish the ticket-id alias via derive_alias(name), then write files through an injected store adapter (JSON file or Postgres).

**Greenfield - onboard::greenfield(store, state_dir, name, alias)** at crates/app/src/onboard.rs:421:
1. Loads state; refuses if tickets exist.
2. Derives (or uppercases) the alias and saves it into state.
3. Writes a default coxagent.json beside the state dir if absent (Config::default()).
4. Writes a blank project_context.md from context_template(name) if absent.
5. Seeds exactly one high-priority Small Feature titled "Walking skeleton" via AddTicketUseCase.
6. Returns an operator message ending in the HUMAN GATE: review/complete project_context.md before running coxagent run.

Greenfield does NOT touch engine/governance config - you get stock defaults.

**Brownfield - onboard::brownfield(store, state_dir, name, alias, codebase)** at crates/app/src/onboard.rs:485:
1. Refuses when codebase.exists() is false or tickets already exist.
2. Adopts the version already declared by manifests via detect_codebase_version(codebase) - the manifest wins over the newest git tag, so releases never regress below what's published.
3. Calls ensure_git_repo(codebase) to init + baseline-commit when not yet .git.
4. Runs comprehension: builds .coxagent/REPO_MAP.md via CodeGraph (build_repo_map) plus stack detection into governance rules (detect_stack, feeding conformance::StackRule).
5. Runs Docker smarts:
   - Compose filenames probed (docker-compose.yml/yaml + compose.yml/yaml) parsed by struct-level helpers into service ports/images/env.
   - Cross-checks against running containers obtained from host-port discovery against well-known ports (KNOWN_SERVICES: postgres 5432 ... vault 8200); classifies each as reusable/clash/missing gap.
   - Findings baked into auto-drafted context (smart_comprehension_context) and used to seed follow-up Chores only when needed ("Dockerize for deploy", "Fix compose - remove duplicate infra", "Add Dockerfile for build").

The hub/dashboard path routes through this same machinery behind an injected factory:
1. HTTP handler checks auth + space membership up front so nothing half-scaffolds (crates/presentation/src/server/projects.rs).
2. It clones your git URL or validates/adopts your local path (system-directory blocks enforced).
3. Greenfield creates <workspace>/<id>/codebase/; local brownfield uses your path in place; git-url import clones into <workspace>/<id>/codebase/.
4. Any confirmed goal is merged onto (never replacing) auto-drafted context at crates/app/src/lib.rs ~line 767.

Architectural note: onboarding performs direct IO inside crates/app (the composition-root / binary wiring crate). AGENTS.md's hexagonal direct-IO ratchet scans only crates/application/src (hexagonal_gate.rs lines 22-56), so this placement does not trip it.

## Usage

Activate from the CLI:

```bash
# Greenfield - scaffold + walking-skeleton seed
coxagent onboard --name "MyApp" --state-dir ~/CoXAgent/myapp/state

# Brownfield - adopt an existing repo on disk
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

Every activation prints/stamps files written plus any seeded tickets and reminds you of the human gate on project_context.md . Re-running against a store that already holds tickets fails with *"workspace already has tickets; refusing to re-onboard"* .

## Interface

CLI subcommand (Command::Onboard, crates/presentation/src/cli.rs):

| Identifier | Kind | Behaviour |
|---|---|---|
| Onboard { name } | CLAP subcommand variant | activates one project |
| Onboard.name     | required long flag String | human-readable project name |
| Onboard.alias    | optional long flag String | short ticket-id alias e.g. CXC; else derived from name |
| Onboard.existing | optional long flag PathBuf | adopt this codebase instead of scaffolding |

Global flags usable on any subcommand include state_dir: PathBuf, default ./state .

HTTP endpoints (crates/presentation/src/server/mod.rs): POST /api/projects -> create_project() returning JSON confirm plus assigned id; GET /api/projects -> list_projects() returning a JSON array plus broken entries.

Request body fields (CreateProjectReq serde struct, crates/presentation/src/server/requests.rs): name required; alias optional; existing optional local path; git_url optional remote to clone; goal optional confirmed brief; space optional admin-gated parent space. Factory input type NewProjectReq at crates/presentation/src/server/mod.rs carries name / alias / existing / git_url / goal.

Pure helper functions in onboard.rs:
- derive_alias(name) -> String at crates/application/src/state/mod.rs:1269 : caps-of-name or first-three-alphanumerics uppercase rule.
- detect_codebase_version(codebase) -> Option<SemVer>
- parse_semver(raw)
- parse_remote(url) -> Option<(provider, owner/repo)>
- ensure_git_repo(dir) -> Result<String, Box<dyn Error>>

## Configuration

Activation itself adds no new runtime config keys beyond what onboarding writes into each new workspace's default coxagent.json:

| Setting | Default on activation |
|---|---|
| engine.default.engine   | Opencode if detected on PATH else Claude (brownfield only) |
| engine.default.model    | bizbrain/DeepSeek-V4-Pro when Opencode detected (brownfield only) |
| engine.auto_fallback    | false (brownfield only) |
| architecture rules      | populated from detected stack manifests (brownfield only) |
| deploy.host_port        | None until later assignment pass |

Green/brown decision knobs are compile-time constants rather than settings: KNOWN_SERVICES port table drives reuse/clash/gap classification; compose filenames probed are docker-compose.yml/yaml + compose.yml/yaml; stack manifests live in the MANIFESTS list inside detect_stack(). Greenfield simply writes Config::default() with no engine/governance overrides.

## Edge cases and limits

Deliberately NOT done:
- No walking skeleton is seeded on brownfield - an adopted app already exists; instead follow-up Chores are added conditionally.
- Brownfield never rewrites or reinitialises your real repo; it only initialises git when the directory has no .git at all (baseline commit on adoption).
- An unparseable compose file is treated as empty (no services) rather than failing activation.
- A docker ps failure returns an empty running-port set, so nothing is misclassified as reusable/clashing.
- Re-onboarding a store that already has tickets is refused outright.
- Git URL import refuses non-git schemes (must start with git@, https:// or http://), enforced in lib.rs ~line 722; the dashboard additionally blocks importing from system directories (/etc, /root, /var/log, /bin, /sbin, /dev, /proc, /sys).

How it fails / limits:
- Version adoption refuses non-semver tags rather than guessing wrong; if no manifest or tag parses, the version stays as-is. Regression tests live in onboard.rs version_adoption_tests (~line 1043).
- Deletion of an imported project removes only CoXAgent scaffolding (state/, coxagent.json) plus a codebase symlink - never the original codebase.

## Code map

- crates/app/src/onboard.rs - greenfield() + brownfield() activation, docker analysis (analyze_docker / running_host_ports / parse_compose / KNOWN_SERVICES), smart context drafting (smart_comprehension_context), seed_smart_tickets, build_repo_map, detect_stack, ensure_git_repo, parse_remote, detect_codebase_version / parse_semver. Unit tests are inline in this module.
- crates/app/src/lib.rs - Command::Onboard dispatch (~line 105) and onboard_project() factory used by the hub/dashboard (~line 693); merges any confirmed goal onto context; unique_id for workspace ids.
- crates/presentation/src/cli.rs - Command::Onboard CLAP shape and flags; global state_dir flag.
- crates/presentation/src/server/mod.rs - GET/POST routes under line 716 plus NewProjectReq type.
- crates/presentation/src/server/projects.rs - list_projects(), create_project() handler with auth + space gating and import-path validation.
- crates/presentation/src/server/requests.rs - CreateProjectReq serde request body.
- crates/application/src/config.rs + config_load.rs - Config::default used to scaffold coxagent.json; host-port healing at load.
- crates/application/src/codegraph.rs - CodeGraph::index consumed by build_repo_map to write REPO_MAP.md.

## Related

CXA-F003 seeds the FEAT walking-skeleton ticket that later RunCycleUseCase advances through BA/Sa/dev/TEST. The comprehension output (.coxagent/REPO_MAP.md) feeds selection/search pages. Deployment happens through run_dev/deploy gates after activation. Related lifecycle endpoints: POST /api/projects/:pid rename (rename_project_ep) and DELETE /api/projects/:pid delete_project_ep in projects.rs. Note there is a build-tree mirror at mergetest/crates/* duplicating some files; treat crates/* as authoritative.

> Reviewer note: this ticket id CXA-F003 collides with docs/CXA-F003.md ("Optimistic concurrency control for cross-runner store saves"), an unrelated feature sharing the same id. The two should be disambiguated before they are linked to from indexes that key on ticket id alone.
