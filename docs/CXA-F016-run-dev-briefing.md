FOLDER: Engineering

# Run-Dev Briefing

**Keywords:** run_dev, ticket brief, task prompt assembly, build_request, knowledge_brief, attempts_brief, BRIEF_PROTOCOL, stale design warning, workspace orientation, prompt caching, DEV-BUG DEV-FEATURE

## Overview

The run-dev briefing is what a developer agent is TOLD before it codes: every block of context that `RunDevUseCase::build_request` strings into the task prompt. It serves two agents at once — a developer who must orient fast on unfamiliar code without rediscovering it via exploratory API rounds, and anyone maintaining the `run_dev` module who needs to know why each block exists and where it lives. The goal is that the FIRST model turn is already oriented: ticket text, prior attempts, organisational knowledge, workspace state and steering arrive pre-assembled instead of being probed one call at a time.

## How it works

`RunDevUseCase<S,E>::execute` (in `crates/application/src/use_cases/run_dev/mod.rs`) claims a ticket then calls `build_request(&state,&id)` to assemble an `AgentRequest`. The assembly lives in `crates/application/src/use_cases/run_dev/briefing.rs`, which combines blocks produced by pure functions in `crates/application/src/prompts.rs` plus three methods defined in briefing.rs itself:

1. **Ticket text** — `ticket_brief(ticket)` renders title + description + technical design approach into the prompt head.
2. **Failure memory** — `<RunDevUseCase>::attempts_brief(state,&id)` reads structured gate rejections from `state.attempt_failures(id)`; when none exist it falls back to prose from `state.ticket_journal`. Each line names the gate and files so a retry clears THAT failure instead of starting over.
3. **Organisational knowledge** — `<RunDevUseCase>::knowledge_brief(...)` builds a query from title + description + approach and calls `prompts::knowledge_block(files,&state.docs,&state.tickets,...)` to pull team wiki pages, project docs and closed tickets on the same subject.
4. **Human steering** — recent USER comments on this ticket become instructions via `prompts::human_steering_block`.
5. **Ask protocol** — `prompts::ask_protocol_block(state,&id.to_string())` tells the agent to ask rather than invent requirements.
6. **Git history block** — `prompts::history_block(...)` surfaces what was already done to this code.
7. **Stale-design warning** — built inline: any file named by the SA design that no longer exists (`fs.stat` returns None) produces a WARNING telling DEV to locate moved code via `.coxagent/REPO_MAP.md` rather than recreate old files.
8. **Workspace orientation block** — built inline via port calls: current branch (`git rev-parse --abbrev-ref HEAD`), dirty-tree porcelain status (first 20 lines), and existence/size of each listed design file (first 12). It ends with "Trust this block instead of re-running ls/git status."
9. **Repo map block** (`prompts::repo_map_block`) — included only when the design does NOT already name real files (checked by whether listed files are non-empty AND nothing was flagged stale).

The system prompt stays byte-identical across every DEV run (`prompts::system_prompt(prompts::DEV)`) so engines keep provider-prompt-cache read pricing; everything per-ticket lives in the task prompt assembled here.

## Usage

To see what an agent will be told for a given ticket without running an engine:

```rust
let state = store.load().await?;
let use_case = RunDevUseCase::new(store.clone(), engine.clone(), config.clone(), work_dir.clone(), DevMode::Feature)
    .with_git(git)
    .with_files(files);
let req = use_case.build_request(&state, &id).await;
println!("{}", req.task_prompt);
```

The assembled task prompt reads roughly:

```
Ticket CXA-F042: Add dark mode toggle
Implement it now.<stack><deploy><design>

PREVIOUS ATTEMPTS on this ticket — each was rejected by a specific gate.
Clear THAT, do not start over:
- attempt 2 — rejected by tests (Gate): test_dark_toggle fails ...

WARNING: this design predates a refactor — these listed files no longer
exist: web/dark.ts. Locate moved code via .coxagent/REPO_MAP.md ...
[workspace orientation / PRIOR WORK / BRIEF protocol blocks]
```

An agent leaves durable memory for later roles by ending its output with one line starting exactly `BRIEF:`; after a successful run its caller extracts these with `prompts::extract_brief_notes(&o.stdout)` and persists them into that ticket's journal.

## Interface

Paths for each identifier are under ## Code map. Briefing-owned methods live on `RunDevUseCase`; helper functions live where noted.

| identifier | purpose |
|------------|---------|
| `<RunDevUseCase>::build_request(state,&id) -> AgentRequest` (briefing.rs) | Assembles every block into one linear task prompt |
| `<RunDevUseCase>::attempts_brief(state,&id) -> String` (briefing.rs) | Failure-memory brief; empty when no failures/journal |
| `<RunDevUseCase>::knowledge_brief(files?,&state,&id,ticket?,&work_dir) async -> String` (briefing.rs) | Organisational knowledge block via port |
| `run_dev::ticket_brief(ticket?) -> String` (mod.rs:1284) | Renders title + description + approach into the prompt head |
| `<ProjectState>::attempt_failures(id)` / `.ticket_journal` / `.design_system`, plus `<Ticket>::has_ui()` (domain ticket.rs:322) | Inputs the blocks read |

Blocks pulled from prompts.rs used inside build_request:

| identifier | purpose |
|------------|---------|
| `prompts::system_prompt(prompts::DEV)` | Byte-stable system prompt for cache-read pricing |
| `prompts::ticket_brief_block(state,ticket)` | PRIOR WORK rendering of last ≤8 journal entries |
| `prompts::human_steering_block(state,ticket_id)` | Recent USER comments as explicit instructions |
| `prompts::history_block(...)` async | What was done to this code previously |
| `prompts::repo_map_block(...)` async | Repo-map orientation; skipped when design names real files |
| `prompts::knowledge_block(...)` async | Wiki + repo docs + closed-ticket search results |
| `prompts::focus_block(...)`, `.stack_constraints`, `.deploy_constraints`, `.design_constraints`, `.team_memory_block_relevant`, `.hub_lessons_block`, `.ask_protocol_block`, `.BRIEF_PROTOCOL` | Sibling blocks assembled into one task |

The returned request also fixes engine-facing fields: role from mode (`Role::DevBug` / `Role::DevFeature`), timeout 3600s, escalation level climbing from stored fail-attempt count capped at 3 (floor raised to rung 1 for Large-complexity tickets), and label set to the ticket id.

## Configuration

No CXA-F016 flags were added; behaviour keys off existing workflow settings:

- System-prompt stability relies on never editing those sections between runs; per-ticket variance must go into blocks joined here or cache-read pricing is lost.
- Repo-map inclusion depends on whether an SA design already names real files (`d.files.iter().any(|f| !f.trim().is_empty())`) AND nothing flagged stale.
- Escalation floor comes from ticket complexity (`Complexity::Large`) combined with stored fail attempts; ceiling fixed at rung 3.
- Port availability degrades gracefully throughout: optional git/files adapters yield empty snapshots rather than errors.

## Edge cases and limits

Deliberately NOT covered here:

- No repo map when SA already named files — if those paths later prove wrong beyond stat-existence checks (wrong but existing file), nothing catches semantic drift.
- Stale-design detection only stats presence; it cannot tell if moves happened between still-existing old/new locations or warn about renamed-but-existing paths not listed in either form until they diverge.
- Orientation's dirty-status list truncates at 20 lines and stat probing at 12 listed files silently rest beyond those caps.
- Knowledge search is best-effort over whatever ports return; missing git/files adapters produce empty history/orientation blocks rather than failing open loudly.
- Attempts/journal render has bounded headroom but stored vectors are unbounded on disk; long-lived hot tickets grow without pruning while rendering stays capped.

Each helper returns empty strings/blocks rather than throwing when data is absent — cold-read behaviour identical before CXA-F016.

## Code map

Real files implementing CXA-F016 (all verified present):

- crates/application/src/use_cases/run_dev/briefing.rs — the assembly core for the DEV agent's task prompt: `<RunDevUseCase>::build_request` strings every block together; also owns `attempts_brief` (failure memory) and `knowledge_brief` (organisational knowledge). Inline logic here builds the stale-design warning and the workspace-orientation block via port calls.
- crates/application/src/prompts.rs — pure-function renderers consumed by `build_request`: `system_prompt`, `ticket_brief_block`, `human_steering_block`, `history_block`, `repo_map_block`, `knowledge_block`, `focus_block`, `stack_constraints`, `deploy_constraints`, `design_constraints`, plus constants like `BRIEF_PROTOCOL`. Reads outside data only through ports.
- crates/application/src/prompts_resolve.rs — per-project system-prompt override resolution (F001 seam): pure precedence between embedded role bodies and a project-local `prompts/<role>.md`. Adjacent to but independent of briefing assembly; useful when tracing why a DEV system prompt differs from `system_prompt(prompts::DEV)`.
- crates/application/src/use_cases/run_dev/mod.rs — declares the three run_dev submodules (`briefing`, `failures`, `gates`) and runs the pass in `<RunDevUseCase>::execute`: claims a ticket, calls [`build_request`](crates/application/src/use_cases/run_dev/briefing.rs), runs the engine, then extracts BRIEF notes from stdout with `prompts::extract_brief_notes` and persists them to that ticket's journal.

Tests:
Renderer unit tests live beside their code inside prompts.rs (inline `mod tests { ... }` plus focused suites such as history_block_tests / knowledge_block_tests), and override-resolution coverage sits in crates/application/tests/prompts_resolve.rs against an in-memory files double with no disk.

## Related

- docs/CXA-F004-per-ticket-brief.md — the durable per-ticket brief layer this page builds on: `BRIEF_PROTOCOL`, `extract_brief_notes`, journal writes, and session resume. F016 is the read-side assembly; F004 is the durable write/read memory it strings in.
- CXA-F003 (docs/CXA-F003.md) — optimistic-concurrency store saves; the brief's journal/knowledge reads go through the same StateStorePort transport hardened there.
- crates/application/src/use_cases/run_dev/failures.rs — how failed attempts become gate-structured records that `attempts_brief` reads back to the next try.
- docs/TEST_COVERAGE_GAP_DETECTION.md and docs/editable_prompt_system.md — adjacent prompt-assembly work touching prompts.rs.
