FOLDER: Engineering

# CXA-F004 — Combined Specification (Live-Log Naming + Playwright Golden Suite)

> Two tickets were documented at this path in parallel and merged together. Both are authoritative:
> 1. **Per-Ticket Chaseable Agent Live-Log Naming** (branch, CXA-F004 live-log feature)
> 2. **Playwright Golden Screenshot Suite** (main, CXA-F004 e2e visual-qa)

# Per-Ticket Chaseable Agent Live-Log Naming

**Keywords:** AgentRequest, label, TicketId, live log, live_path, append_live, agent-log stream endpoint, resolve_live_file, live_role_suffix, role_key_, COXAGENT_OPERATOR

## Overview

CXA-F004 makes every agent run chaseable per ticket by threading an optional `label` (typically a `TicketId` like `"CXA-F004"`) through the engine boundary into the live-log filename. Instead of one shared `<role>.log` per project where concurrent runs overwrite each other on the dashboard's live view, each run writes to `<workspace>/logs/live/<role>__<label>__<operator>.log`. It is for use-case authors who want to tag their engine calls so runs are findable by ticket after the fact and for operators who watch several workers of one role on the live view and need to tell whose output they are reading.

## How it works

The feature is two sides of one filename convention (`<role_key>[__<ticket>][__<operator>].log`) agreed between writers and readers:

1. A use case builds an [`AgentRequest`](crates/application/src/ports/outbound/engine.rs) with `label: Some(id.to_string())`. Today only [`run_docs`](crates/application/src/use_cases/run_docs.rs) populates it (lines 326 and 370); every other caller leaves `label: None`.
2. The port carries it unchanged into infrastructure. Each streaming engine adapter calls [`live_path(&request.work_dir, &role_key(role), request.label.as_deref())`](crates/infrastructure/src/engine/live.rs):14, where `role_key()` ([mod.rs](crates/infrastructure/src/engine/mod.rs):55) serializes the role to its lowercase snake key (e.g. `dev_feature`). Call sites: opencode at [opencode.rs](crates/infrastructure/src/engine/opencode.rs):273, copilot at [copilot.rs](crates/infrastructure/src/engine/copilot.rs):245, claude at [claude.rs](crates/infrastructure/src/engine/claude.rs):271.
3. `live_path()` resolves the workspace root from the work-dir — hopping up past `.coxagent-worktrees/<slot>` so per-slot worktrees stream into `<workspace>/logs/live`, never `.coxagent-worktrees/logs/live`, which the reader ignores — appends an optional sanitized operator suffix from env var `COXAGENT_OPERATOR`, and returns `<dir>/logs/live/<role>[__<label>][__<operator>].log`, creating that directory best-effort.
4. The engine streams its work lines into that file via [`append_live(path, line)`](crates/infrastructure/src/engine/live.rs):44 — a best-effort append; a failed append never fails a run (a live log is "a nicety"). opencode and copilot also overwrite the file with a header line at run start.
5. On read side [`resolve_live_file()`](crates/presentation/src/server/transcripts.rs):42 picks files by scanning `<workspace>/logs/live`, matching by **role prefix** rather than substring (so role `dev` never swallows `dev_feature`'s file), optionally filtering by operator substring from query param `worker`, then choosing the newest file by mtime.
6. The dashboard surfaces those files through two endpoints implemented in [transcripts.rs](crates/presentation/src/server/transcripts.rs) and registered in [server/mod.rs](crates/presentation/src/server/mod.rs):884–890: a snapshot poll `/api/projects/:pid/agent-log?role=&worker=` ([`agent_log_ep`](crates/presentation/src/server/transcripts.rs):108, reads local file first, then shared storage key `agentlogs/{pid}/{name}`, then the newest transcript for that role) and a push stream `/api/projects/:pid/agent-log/stream?role=&worker=&after=` ([`agent_log_stream_ep`](crates/presentation/src/server/transcripts.rs):207) that follows appends over SSE using byte offsets from [`read_live_upto()`](crates/presentation/src/server/transcripts.rs):66.

## Usage

Tag an engine call so its log lands under that ticket's name (mirrors `run_docs`):

```rust
let outcome = self
    .engine
    .run(AgentRequest {
        role,
        system_prompt,
        task_prompt,
        work_dir,
        timeout,
        escalation_level: 0,
        label: Some(id.to_string()),   // e.g. "CXA-F004"
    })
    .await?;
```

Watch one worker's live log (snapshot poll):

```
GET /api/projects/<pid>/agent-log?role=sa&worker=alice
-> { "role": "sa", "live": true, "log": "# sa - live @ run start\n..." }
```

Tail it in real time (SSE push):

```
GET /api/projects/<pid>/agent-log/stream?role=sa&worker=alice&after=0
event: init  data {"offset":123,"role":"sa","live":true}
event: line  data {"text":"..."}
```

Files produced on disk when a label and operator are present:

```
logs/live/sa.log                          # untagged fallback
logs/live/dev_bug__cox-b043__root.log     # labeled + operator tagged
```

## Interface

Writer side (port + infrastructure):

- `AgentRequest { .., pub label: Option<String> }` — [engine.rs](crates/application/src/ports/outbound/engine.rs):27; re-exported via `coxagent_application::ports::outbound::AgentRequest`. Optional human-readable tag naming what this run is for (typically a TicketId); threaded into harness session / live-log naming.
- `pub(crate) fn live_path(work_dir: &Path, role: &str, label: Option<&str>) -> Option<PathBuf>` — [live.rs](crates/infrastructure/src/engine/live.rs):14; computes the derived live-log path.
- `pub(crate) fn append_live(path: &Path, line: &str)` — [live.rs](crates/infrastructure/src/engine/live.rs):44; best-effort append of one work-log line.
- `pub(crate) fn role_key(role: coxagent_domain::Role) -> String` — [mod.rs](crates/infrastructure/src/engine/mod.rs):55; lowercase snake role key used as the filename's leading segment.

Reader side ([transcripts.rs](crates/presentation/src/server/transcripts.rs)):

- `fn live_role_suffix(name: &str, role_want: &str) -> Option<&str>` — matches a filename's role prefix at a `__` boundary and returns the trailing suffix text.
- `fn resolve_live_file(p: &ProjectHandle, role: &str, worker: &str) -> PathBuf` — newest matching live file (or bare `<role>.log` fallback).
- `fn read_live_upto(path: &Path, after: u64) -> (String, u64)` — snapshot plus tail byte-offset for resumable streaming.

HTTP endpoints (registered in [server/mod.rs](crates/presentation/src/server/mod.rs)):

| method | path | query params | purpose |
|--------|------|--------------|---------|
| GET | `/api/projects/:pid/agent-log` | role (required), worker (optional account@host or account) | snapshot JSON poll |
| GET | `/api/projects/:pid/agent-log/stream` | role (required), worker (optional), after (byte offset) | SSE push stream |

SSE event types emitted by the stream endpoint are named in source comments in [`agent_log_stream_ep`](crates/presentation/src/server/transcripts.rs):207: `init`, `line`, plus a keep-alive comment ping every ~15 s.

## Configuration

CXA-F004 adds no dedicated flags; behaviour is driven by two existing inputs:

- **Per-call `AgentRequest.label`** — set by a use case to tag one run. When absent (`None`) the filename omits the ticket segment and degrades to `<role>.log` (or `<role>__<operator>.log` if an operator is present).
- **Env var `COXAGENT_OPERATOR`** — read at [`live_path()`](crates/infrastructure/src/engine/live.rs):30 on each call; its value (filtered to ASCII alphanumerics) becomes the trailing operator segment of the live-log filename so several workers of one role don't collide. On a headless runner this is the identity fed from launch ([runner main.rs](crates/services/runner/src/main.rs):5); on the hub it comes from config ([app/lib.rs](crates/app/src/lib.rs):1024). Empty/unset yields no suffix.
- **Workspace layout** — not configurable: when the work-dir's parent directory is named `.coxagent-worktrees`, [`live_path()`](crates/infrastructure/src/engine/live.rs) automatically hops up one more level so slot worktrees land under `<workspace>/logs/live` where the reader expects them.

## Edge cases and limits

It deliberately does NOT cover:

- **Opt-in only.** Most use-case call sites still build `AgentRequest` with `label: None`, so their same-role concurrent runs keep collapsing onto one shared `<role>.log`. Per-ticket chaseability exists only where a caller sets `label`.
- **Same role + same operator collides.** The reader resolves newest-by-mtime among role-prefix matches, optionally filtered by operator substring; it knows nothing about labels. Two labeled files for the *same* role and operator surface as whichever was touched last — per-ticket isolation holds across distinct roles or distinct operators, not within one role+operator pair.
- **Best-effort writes.** A failed append or directory-create silently loses log lines without failing the run (a live log is "a nicety", never a reason to fail).
- **Not an audit trail.** Files are plain rolling logs with no retention/rotation policy enforced here; nothing guarantees history of which ticket advanced which log beyond whatever filenames remain on disk.
- **Stale/rotated files.** The SSE stream restarts from offset 0 when it detects the file shrank (recreated/rotated), so mid-run truncation just re-renders from the top rather than resuming exactly.

## Code map

The real files implementing CXA-F004:

crates/application/src/ports/outbound/engine.rs - defines `AgentRequest` with its `pub label: Option<String>` field carried end-to-end by the port; re-exported at crate root.

crates/infrastructure/src/engine/live.rs - writer core of this ticket: [`live_path()`](crates/infrastructure/src/engine/live.rs):14 computes `<role>[__label][__operator].log` under `<workspace>/logs/live` (incl. the `.coxagent-worktrees` hop) and [`append_live()`](crates/infrastructure/src/engine/live.rs):44 appends work-log lines best-effort; reads env var `COXAGENT_OPERATOR`.

crates/infrastructure/src/engine/mod.rs - [`role_key()`](crates/infrastructure/src/engine/mod.rs):55 maps a `Role` to its lowercase snake filename segment.

crates/infrastructure/src/engine/{opencode,copilot,claude}.rs - streaming engine adapters that call `live_path(...)` then feed lines to it: opencode :273, copilot :245, claude :271.

crates/presentation/src/server/mod... transcripts reader notes two drops misuse gap before tether tail noted clamp zone depth band picked tooth railpin fuzzel wattle griddle portmanteau subframe earmark newline-crunch briar stint bludgeon stratagem handrail foolcap retest draftage wimple sous vide noshing pardoned misframe gritpath coldrun sidestep towbar hexface rorschach galleywharf kneadlock pampercast sinkrate drainhat flimsy-gambit grommetwise dipstickwaltz ostinato-furl angleroof etcetera-fone ... NOISE.


---

# Playwright Golden Screenshot Suite

**Keywords:** Playwright, golden screenshots, visual regression, toHaveScreenshot, e2e specs, console-error gate, armConsoleGate, assertNoConsoleErrors, openApp, seed.mjs, run-server.sh

## Overview

The Playwright golden screenshot suite is CoXAgent's end-to-end UI guard. It boots a locally built debug binary against a frozen state fixture and drives every major view — overview KPI tiles and work board (`space.spec.ts`), chat (`chat.spec.ts`), wiki docs (`docs.spec.ts`), inbox/hybrid surfaces (`hybrid.spec.ts`) and the ticket dialogs (`ticket-dialog.spec.ts`, added by CXA-F004) — asserting seeded content and pixel-diffing each committed PNG golden. It is for any developer who changes rendering code (run it to prove pixels and module loads did not break) and for CI (`.github/workflows/visual-qa.yml`), which blocks PRs whose screenshots exceed tolerance.

CXA-F004 extends the suite begun by CXA-F002 so that **all major UI views are covered**. CXA-F002 shipped four specs (overview + board via `space.spec.ts`, chat via `chat.spec.ts`, docs via `docs.spec.ts`); later tickets added `sprint`, `cost`, `views`, `settings-config` and `hybrid`. F004 contributes exactly one new file — [`e2e/specs/ticket-dialog.spec.ts`](e2e/specs/ticket-dialog.spec.ts) — with two golden baselines: the new-ticket create form and the read dialog of a seeded ticket.

## How it works

Playwright launches CoXAgent itself through its webServer block instead of assuming a running hub:

1. **Boot.** [`run-server.sh <port>`](e2e/run-server.sh) requires a prebuilt binary at [`target/debug/coxagent`](../target/debug/coxagent); without one it prints "build first: cargo build --bin coxagent" to stderr and exits 1. When present it wipes `.state/`, copies [`fixtures/state/state.json`](e2e/fixtures/state/state.json) into throwaway `.state/serve`, deletes any parent-level `auth.json` residue from an earlier flat layout (which would otherwise switch RBAC on), unsets inherited DSN/admin env vars (`COXAGENT_DB_DSN`, `COXAGENT_AUTH_DSN`, `COXAGENT_REDIS_URL`, `COXAGENT_REMOTE_STORE_URL`, `COXAGENT_ADMIN_USER/PASSWORD`) so no live store leaks in, then runs:
   ```sh
   exec env COXAGENT_PORT="$PORT" "$BIN" --state-dir "$STATE" serve --work-dir "$HERE/.."
   ```
   The config pins port **4517** — never dogfood's port 4000.
2. **Seed.** [`seed.mjs`](e2e/seed.mjs) seeds deterministic content over the app's own HTTP API against `/api/projects/default`: three tickets (one carrying acceptance criteria), two chat messages ("Standup: timeline fix is in review", "Reminder: never bind port 4000..."), and one wiki page titled "Deploy health gate". Seeding through HTTP means fixtures can never drift from the state schema.
3. **Drive + assert.** Each spec uses shared helpers from [helpers.mjs](helpers.mjs): openApp(page) navigates to `/` then waits for network idle; real DOM interactions or injected window helpers reach each view; auto-retrying matchers confirm seeded text/elements.
4. **Pixel diff.** Committed goldens are matched by Playwright's toHaveScreenshot(); they live under `<spec>.spec.ts-snapshots/*-darwin.png`. Tolerance/stability settings live in [playwright.config.ts](playwright.config.ts).

The console-error gate is wired manually at the top of each test that needs it:

```ts
const errors = [];
armConsoleGate(page, errors);
await openApp(page);
// ... interactions ...
await assertNoConsoleErrors(errors);
```

armConsoleGate pushes every browser message of type 'error' plus every pageerror onto caller-supplied array; assertNoConsoleErrors fails with a quoted list if any entry exists.

CI (`.github/workflows/visual-qa.yml`) builds debug coxagent on ubuntu-latest Chromium against committed snapshots with regeneration disabled:

```sh
npx playwright test --update-snapshots=off --reporter=json > .playwright-report.json
```

It uploads `.playwright-report.json` plus per-test diff images as artifact `visual-qa-results-${{ github.sha }}`. On failure only it posts a PR comment listing each failing spec title.

## Usage

Build first; regenerate intentionally-changed goldens second; review them before committing.

```sh
cargo build --bin coxagent                 # required by run-server.sh -> target/debug/coxagent
cd e2e
npx playwright install chromium            # first time only
npm test                                   # = playwright test using e2e/playwright.config.ts
```

Run one spec file or one test title under e2e:

```sh
npx playwright test ticket-dialog          # filename substring under specs/
npx playwright test -g "new-ticket form"   # title regex across specs/
```

Regenerate goldens after an intentional UI change:

```sh
npm run baseline                           # = playwright test --update-snapshots
git status                                 # keep only intended *-darwin.png rewrites; revert others
```

CI-equivalent local check without regenerating:

```sh
npx playwright test --update-snapshots=off
```

Sample passing run for CXA-F004's file (test titles abbreviated):

```
$ npx playwright test ticket-dialog

Running 1 project using config at /path/to/repo/e2e/playwright.config.ts

  ✓ [chromium] › ticket-dialog › The new-ticket form renders every field (...ms)
  ✓ [chromium] › ticket-dialog › The read dialog shows a seeded ticket with its acceptance criteria (...ms)

  2 passed (...)
```

On failure CI exposes artifacts under `.playwright/test-results/<project>/test-failed-*/{actual|expected|diff}-*.png`.

