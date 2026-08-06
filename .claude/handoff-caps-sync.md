# Handoff: POST /api/caps - local capability sync to hub

Status: SPEC ONLY, not implemented. This cleanly supersedes an earlier corrupted
draft whose code recipe was unusable; the anchors below are re-verified against
the current tree.

## Goal

A local machine detects its own engines/models/tooling/git and syncs them to the
hub over HTTP, so the dashboard (including one served from a container with no
CLI) shows real runner-local capability, and can later dispatch work back to that
runner. This is Part A; Part B (dispatch tickets over HTTP to the runner) is done
separately.

## Why /api/engines returns [] on a container

`engines_ep` (crates/presentation/src/server/engines.rs:198) reads only:
1. `app.engines` - scan of PATH of the hub process;
2. `p.store.workers()` per project - worker registry with TTL.

A hub in a split-compose container has no opencode on PATH and no worker heartbeat,
so the list is empty. We need an inbound channel for a local machine to report caps
into that worker registry, which /api/engines and /api/engines/opencode/models both
already read.

## Ground truth (verified in-tree)

- WorkerEntry fields @ crates/application/src/ports/outbound/state_store.rs:14
  { worker, role, ticket, at, engines: Vec<String>, models: Vec<String>,
    git: Option<GitCheck>, tooling: Option<serde_json::Value> }
- WorkerCaps struct @ same file line 94 { engines, models, git, tooling }.
- GitCheck struct @ line 58 { account, api_ok, push_ok, detail, remedy }, derives Default.
- Trait method @ state_store.rs:223:
   async fn heartbeat_worker(&self,_worker,&str,_role,&str,_ticket,&str,
     _caps,&WorkerCaps,_now,&str) -> Result<(),PortError>
  Default no-op; json_store impl writes + prunes by WORKER_TTL_SECS.
- json_store heartbeat_blocking @ crates/infrastructure/src/state/json_store.rs:~264 .
- Loop-projects pattern (copy verbatim): clone handles out then iterate:
   let projects = app.projects.read().await.clone();
   for p in projects.values() { ... p.store ... .await ... }
  See engines.rs:207 and opencode_models_ep engines.rs:253.
- RFC3339 now pattern @ server/mod.rs:~203.

## Auth template to mirror - POST /api/pr-report

Layout reference only (not scaled): handler ~ forge.rs:914 with a flat path route
registered ~ mod.rs:949. POST with JSON body on a plain path (no :pid segment).
Runs through auth_mw like every /api/* -> authenticated by an internal bearer token;
when auth is off (app.auth == None) auth_mw passes through so local sync still works.
Token minted similarly to builders ~702 with a label like internal:caps:{machine},
at the right AuthRole.

## Implementation - Part A (inbound endpoint only)

### A1 handler + route

1. New flat module `caps_report` in crates/presentation/src/server/, handler
   `pub(super) async fn caps_report_ep`; register the route before the auth_mw
   route_layer like every other /api/* route (mod.rs:959).
2. Request body:
   { "worker": "account@host",          // required operator identity
     "role": "agent",                   // role label
     "ticket": "",                      // optional ; usually empty when idle
     "engines": ["opencode"],
     "models": ["anthropic/claude"],
     "git": {...GitCheck fields...},
     "tooling": { ... } }               // opaque, for dashboard display
3. For each live project call p.store.heartbeat_worker(worker, role, ticket,
   &caps, now). Loop all projects (mirror of pr-report) so a machine that does not
   yet know its pid still appears everywhere; scope to one project later.
4. Write one audit line per report.

### A2 client-side reporter infra (later)

Mirror HttpPrReporter + PrReporterPort: a reporter that detects local engine /
model / git / tooling and posts it to COXAGENT_REMOTE_STORE_URL when set. Build the
port next to build_pr_reporter in crates/app/src/builders.rs.

## Verification

| Slice | Check |
|---|---|
| Endpoint compiles | cargo check -p coxagent-presentation |
| Auth off    | POST /api/caps passes through; worker shows in GET /api/engines |
| Auth on      | POST without internal bearer -> 401/403 from auth_mw |

Open decision: loop-all-projects vs single-project scope. Start with loop-all so it
works end to end before scoping.
