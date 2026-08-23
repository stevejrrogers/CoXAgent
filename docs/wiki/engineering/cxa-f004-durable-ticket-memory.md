FOLDER: Engineering

# Durable Ticket Memory (CXA-F004)

**Keywords:** ticket_journal, journal_note, ticket_brief_block, BRIEF protocol, extract_brief_notes, PRIOR WORK block, engine-agnostic memory, role handoff SA DEV TEST PD

## Overview

CXA-F004 gives every role (SA / DEV / TEST / PD) durable memory of what EARLIER work on a ticket already tried and found — so a retried or handed-off ticket resumes from prior findings instead of rediscovering them from scratch. It spans two complementary mechanisms: a per-ticket work **journal** (`ticket_journal`) appended to by `journal_note()` and read back as a "PRIOR WORK ON THIS TICKET" prompt block via `ticket_brief_block()`, plus the lightweight **`BRIEF:`** marker protocol agents can leave in their own output which gets extracted (`extract_brief_notes`) and persisted into that same journal. It is for anyone changing how an agent task prompt is assembled or how an agent outcome is persisted.

## How it works

The durable source of truth is one field on `ProjectState` (`crates/application/src/state/mod.rs`): `pub ticket_journal: BTreeMap<String, Vec<String>>`, declared at state/mod.rs:304 (defaulted empty at :435). Each key is a ticket id; each value is an ordered list of short notes.

1. **Writing.** Code calls `journal_note(&mut self ,ticket ,note)` defined at state/mod.rs:631. It trims each note to 500 chars, drops empty notes entirely so garbage never lands in the journal, appends to that ticket's vector, then drains overflow so at most 4 notes survive per ticket.

2. **Reading.** `ticket_brief_block(state ,ticket)` at prompts.rs:659 pulls that ticket's notes out of the map; when there are none it returns an empty string so no block renders. Otherwise it takes the newest 8 (newest last) and renders a PRIOR WORK ON THIS TICKET block with one dash-and-note line per note.

3. **Role-specific briefs.** The DEV use case does not call `ticket_brief_block` directly; instead `RunDevUseCase::attempts_brief(state ,id)` at run_dev/briefing.rs:47 prefers structured rejection records from `state.attempt_failures(id)` when any exist, falling back to the plain journal. The SA use case calls `prompts::ticket_brief_block` directly inside its memory block at run_sa.rs:179.

4. **The BRIEF protocol.** Task prompts carry the const `BRIEF_PROTOCOL` at prompts.rs:682, telling agents that if they learned something the next role must know — a gotcha, a decision and why, an approach that failed — end with a line starting with literal "BRIEF:" .

5. **Extracting and persisting briefs.** After a successful run both SA and DEV call `extract_brief_notes(stdout)` at prompts.rs:691, which collects trimmed non-empty text following each "BRIEF:" marker line (each capped at 400 chars). Every extracted note is then persisted back through the journal as role-prefixed text inside the port-level retry helper [`mutate_state`](crates/application/src/ports/outbound/state_store.rs) so concurrent runners never clobber each other.

6. All per-ticket blocks ride in task_prompt ; system_prompt stays byte-identical for cache pricing .

## Usage

An agent ends its output with one BRIEF line:

    END OUTPUT WITH ONE LINE:
    BRIEF      design lives in shared/core/mind ; src/lib was renamed away

On its next run any role sees prior context without asking anyone:

    PRIOR WORK ON THIS TICKET (earlier runs ...):
    - SA      design lives ...

Round-trip proving write-to-read, and the extraction test, both ship as Rust unit tests inside prompts.rs (:1209 and around) and state/mod.rs (:1476). See Code map below for exact locations.

## Interface

Journal mutation and accessors on ProjectState (`crates/application/src/state/mod.rs`):

- field `pub ticket_journal: BTreeMap<String, Vec<String>>` — declared at :304; defaulted empty at :435.
- `pub fn journal_note(&mut self ,ticket :&str ,note :&str )` — defined at :631; append a bounded note (500 chars max each; at most 4 kept per ticket); empty notes are ignored.

Prompt builders in the prompts module (`crates/application/src/prompts.rs`):

- `pub fn ticket_brief_block(state ,ticket) -> String` — at :659; renders the PRIOR WORK block or an empty string.
- `pub const BRIEF_PROTOCOL: &str` — at :682; marker instructions appended to task prompts.
- `pub fn extract_brief_notes(stdout) -> Vec<String>` — at :691; pulls "BRIEF:" lines (400 char cap each).

DEV briefing helper in run_dev/briefing.rs:

- `pub(super) fn attempts_brief(state ,id) -> String` — at :47; DEV's own brief; structured gate-rejection records when present, else the raw journal lines.

## Configuration

No runtime flags or config keys gate this feature ; behaviour derives purely from state plus prompt composition. The only tunables are hard-coded bounds inside those functions :

- 500 chars per note — inside `journal_note()` .
- at most 4 notes kept per ticket (oldest drained) .
- at most 8 notes rendered by `ticket_brief_block()` .
- `extract_brief_notes()` caps each extracted line at 400 chars .

## Edge cases and limits

It deliberately does NOT cover :

- Cross-engine resume of code state — the journal records what an attempt found (a gotcha, a decision) , not a diff or snapshot of the working tree ; actual code knowledge still comes from repo_map / knowledge blocks on each run .
- Prompt-cache stability of per-ticket blocks — these ride in task_prompt (recomputed each run) , so only system_prompt is cache-stable ; that trade-off is intentional .
- An empty or whitespace-only note is silently dropped rather than stored as noise .
- Journal clearing on ticket completion happens elsewhere in the ticket lifecycle (for example run_dev/mod.rs:914 and :1021 remove the key when work ships) ; this feature itself only appends and trims .

## Code map

Real files implementing CXA-F004 :

- crates/application/src/state/mod.rs — `ProjectState.ticket_journal` field (:304) and `journal_note()` (:631); test `journal_note_bounded_and_capped` (:1476) .
- crates/application/src/prompts.rs — `ticket_brief_block` (:659), `BRIEF_PROTOCOL` (:682), `extract_brief_notes` (:691); tests at :1193-1219 (`brief_notes_are_pulled_from_the_marker_lines_only`, `ticket_brief_block_renders_prior_work_or_nothing`) .
- crates/application/src/use_cases/run_dev/briefing.rs — `attempts_brief` (:47) and the task_prompt assembly in `build_request` (steering + journal + BRIEF_PROTOCOL; system_prompt stays byte-stable) .
- crates/application/src/use_cases/run_sa.rs — `ticket_brief_block` in the memory block (:179); extract + persist briefs via mutate_state/journal_note "SA: ..." (:221-226) .
- crates/application/src/use_cases/run_dev/mod.rs — DEV extract + persist loop "{role_tag}: ..." via mutate_state/journal_note (:500-510) .
- Other journal_note callers : use_cases/cycle/escalation.rs:366 ("rescue"), use_cases/cycle/forge_merge.rs:364 ("human closed PR ... unmerged"), use_cases/run_dev/failures.rs:98 ("attempt N failed") .
- crate::ports::outbound::state_store::mutate_state — the concurrent-save retry helper used to persist briefs .

## Related

CXA-F003 (docs/CXA-F003.md) implements optimistic concurrency on the state store ; this feature reuses its shared helper mutate_state for conflict-safe persistence.
