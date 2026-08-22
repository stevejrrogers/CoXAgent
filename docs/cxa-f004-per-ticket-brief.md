FOLDER: Engineering

# Portable Per-Ticket Brief

**Keywords:** ticket brief, BRIEF protocol, extract_brief_notes, ticket_brief_block, ticket journal, journal_note, context reuse, session resume, role handoff, build_request, knowledge_brief

## Overview

CXA-F004 makes a working agent's findings survive its run as durable, engine-agnostic ticket memory. A session resume (`ticket_sessions`) is one engine's private conversation history; it dies on an engine switch, a failover to another runner or machine, or a role handoff (DEV → TEST → SA). The portable per-ticket brief is the layer that survives all three: any role writes notes to the ticket's own journal and every later role reads that journal before starting. It is for anyone maintaining `RunDevUseCase` or `RunSaUseCase`, and for anyone tuning prompt cost and context reuse.

## How it works

Every ticket carries a durable journal on the aggregate — `ProjectState::ticket_journal`, a `BTreeMap<TicketId -> Vec<String>>` appended through [`journal_note`](crates/application/src/state/mod.rs). Two directions use it:

**Writing.** An agent's task prompt ends with [`prompts::BRIEF_PROTOCOL`](crates/application/src/prompts.rs), which instructs the model to finish with one line starting exactly `BRIEF:`. After a successful run the caller scans stdout with [`prompts::extract_brief_notes`](crates/application/src/prompts.rs), tags each note with its role (`"DEV-BUG:"`, `"SA:"`, ...), and persists them via [`journal_note`](crates/application/src/state/mod.rs) inside an atomic read-modify-write (`mutate_state`). Both callers capture on success:

- DEV-BUG + DEV-FEATURE in [`run_dev/briefing.rs::build_request`](crates/application/src/use_cases/run_dev/briefing.rs) via `extract_brief_notes(&o.stdout)`.
- SA design gate in [`run_sa.rs::execute`](crates/application/src/use_cases/run_sa.rs).

**Reading.** When building a task for any role on that same ticket:

- [`prompts::ticket_brief_block(state, id)`](crates/application/src/prompts.rs) renders up to the last 8 journal entries as a "PRIOR WORK ON THIS TICKET" block. DEV calls it from `build_request`; SA calls it so a bounced-back design carries what DEV/TEST learned instead of re-deriving it.
- `<RunDevUseCase>::attempts_brief(state, id)` covers failure memory separately — structured gate rejections from `attempt_failures()`, falling back to prose journal when none exist.
- `<RunDevUseCase>::knowledge_brief(...)` assembles organisational context (team wiki + repo docs + closed tickets on the subject) via `prompts::knowledge_block`.

All blocks assemble into one linear task prompt whose system prompt stays byte-identical across runs so engines keep provider-prompt-cache read pricing; every per-ticket bit lives in this task prompt.

## Usage

An agent leaves durable memory by ending its output with a line starting exactly `BRIEF:`:

```
# end of some DEV run's output
...
Local integration test needs PG running; set COXAGENT_TEST_PG_DSN before green.
BRIEF: /api/projects/<pid>/store ignores stale revision when data omitted.
```

On success this becomes part of the next reader's task prompt automatically — nothing else is required of DEVs or SAs; both callers wire read/write internally.

Inspect what survived by reading state directly:

```rust
let state = store.load().await?;
assert!(state.ticket_journal["CXA-F003"].iter().any(|n| n.starts_with("DEV-BUG:")));
```

## Interface

Prompt renderers and helpers (all identifiers below are defined in files named under ## Code map):

| identifier | purpose |
|------------|---------|
| `prompts::BRIEF_PROTOCOL` | Appended instruction telling an agent to end with one line starting exactly `BRIEF:` |
| `prompts::extract_brief_notes(stdout)` | Pulls lines prefixed exactly `BRIEF:`; trims each note to 400 chars |
| `prompts::ticket_brief_block(state,ticket)` | Renders last ≤8 journal entries as PRIOR WORK block; empty string when none |
| `<ProjectState>::journal_note(ticket,&str)` | Appends one tagged note to that ticket's journal |

Builder methods on the agent flows:

| identifier | purpose |
|------------|---------|
| `<RunDevUseCase>::build_request(state,&id)` -> AgentRequest | Assembles every block including brief/knowledge/journal/stale-design warning |
| `<RunDevUseCase>::attempts_brief(state,&id)` -> String | Failure-memory brief (gate rejections or prose fallback) |
| `<RunDevUseCase>::knowledge_brief(...)` async -> String | Organisational knowledge block |

Consumers (both read AND write): DEV-BUG + DEV-FEATURE via briefing.rs; SA design gate via run_sa.rs.

## Configuration

No new configuration flags were added by CXA-F004. Behaviour keys off existing workflow settings:

- Prompt-cache behaviour relies on keeping the system prompt stable (`prompts::system_prompt(prompts::DEV / TEST / ...)`) unchanged across runs.
- Journal capacity is enforced only at render time (`rev().take(8)` in `ticket_brief_block`) and notes are capped at 400 chars at capture time in both writers.
- Whether engines support session resume does not change anything here; this layer exists precisely so context survives when they do not.

## Edge cases and limits

Deliberately NOT covered:

- **Not retroactive.** Notes persist only after CXA-F004 ships and only for agents that follow the protocol; older output produces nothing.
- **Journal growth is unbounded on disk.** Rendering caps at 8 entries but appends never prune — long-lived hot tickets grow their stored vector without bound.
- **Role tagging is cosmetic.** Writers prefix notes but readers treat them uniformly; there is no access control over who may add or read entries.
- **No de-duplication.** Re-entering the same failed approach can append near-duplicate BRIEF lines rather than replacing them.
- **Best-effort persistence.** Capture happens inside best-effort mutations whose failures are logged rather than fatal; an interrupted write loses that run's notes silently.
- Failed runs record structural failures instead of BRIEF notes — failure memory goes through attempts/gate records, not this channel.

Absence degrades quietly: if nothing was written/extracted these functions return empty strings/blocks rather than throwing — cold-read behaviour identical to before CXA-F004. The IO discipline holds throughout: all these are pure functions over port-fetched data behind ports; writes go through StateStorePort mutations.

## Code map

Real files implementing CXA-F004 (all verified present):

- crates/application/src/prompts.rs — prompt renderers for the brief: `BRIEF_PROTOCOL`, `extract_brief_notes`, `ticket_brief_block`, plus `knowledge_block` and the sibling blocks (`focus_block`, `repo_map_block`, `history_block`, `human_steering_block`) that `build_request` strings together.
- crates/application/src/use_cases/run_dev/briefing.rs — DEV side of assembling each task: `build_request` strings together every block (including `prompts::ticket_brief_block`, knowledge, journal, steering), plus failure memory in `attempts_brief` and organisational context in `knowledge_brief`.
- crates/application/src/use_cases/run_dev/mod.rs — runs the DEV pass (`execute`); after a successful engine run it extracts BRIEF notes with `prompts::extract_brief_notes(&o.stdout)` and persists them into the ticket journal.
- crates/application/src/use_cases/run_sa.rs — SA design gate: reads the ticket brief block at run start and persists its own SA-tagged BRIEF notes on success.
- crates/application/src/state/mod.rs — durable journal storage on the aggregate: `ticket_journal` field, persisted through StateStorePort, with appends via `<ProjectState>::journal_note`.

Tests:
- Unit coverage for the renderers lives beside them in prompts.rs (e.g. tests that assert prior-work rendering) and state-mod tests cover journal append semantics.

## Related

- CXA-F003 (docs/CXA-F003.md) — optimistic-concurrency store saves; both tickets share `/api/projects/:pid/store` transport work tracked in `.claude/handoff-rest-runner.md`. The brief's writes go through the same atomic StateStorePort mutations that F003 hardened.
- Tầng 1 session resume (`ticket_sessions`) is the other half of per-ticket context reuse; this page is Tầng 2, which survives engine switch / failover / role handoff where resume cannot. See run_dev/mod.rs around session capture/resume.
- Test Coverage Gap Detection (docs/TEST_COVERAGE_GAP_DETECTION.md) and editable prompt system (docs/editable_prompt_system.md) touch adjacent prompt-assembly code in prompts.rs.
