# Adaptive approval: the gate that moves itself

**Keywords:** approval, autonomy, risk, confidence, auto-approve, undo window,
preference learning, batch approval, #approvals channel, hybrid team

## Overview

The hybrid gates (see [HYBRID_TEAM.md](HYBRID_TEAM.md)) put a person in front
of every designed ticket. That is right on day one and wrong by day ten: on a
real board most decisions are routine — "another test-coverage ticket, same
shape as the last eight, all approved". A human clicking Approve twenty times
is a rubber stamp, and a rubber stamp is not governance, it is latency.

Adaptive approval keeps the human where judgement is actually needed and lets
the team move everywhere else. Three mechanisms, in order of leverage:

1. **Risk-scored auto-approval with an undo window** — routine work proceeds,
   announced rather than asked; a person can pull it back.
2. **Preference learning** — the boundary between "ask" and "proceed" is
   learned from what this team's humans actually approve and reject.
3. **Batched, themed approval** — what still needs a human arrives grouped,
   deduplicated, and decidable in one read.

The one-line rule: **agents handle the routine, humans handle the exception,
and the line between them moves as the machine learns this team's taste.**

## How it works

### 1. Risk score and the undo window

At design time every ticket gets a score from data the system already holds —
no model call, no guesswork:

| Signal | Raises risk | Lowers risk |
|---|---|---|
| Complexity | `large` | `small`, `medium` |
| Files in the technical design | sensitive paths (workflows, Dockerfile, compose, `scripts/`, `Cargo.toml`, `coxagent.json`), or many files | a handful of ordinary source files |
| Acceptance criteria | none | present and specific |
| Ticket type | feature touching runtime behaviour | test coverage, docs, chore |
| Prior art | no similar ticket ever shipped | N similar tickets shipped and verified |
| Ticket history | previously parked or rejected | clean |

The score picks a lane:

- **Low risk** → the ticket moves to `Ready` immediately and posts one line to
  `#approvals`: *"F033 auto-approved (routine test coverage, same shape as
  F021/F026). Undo within 30 minutes."* An **Undo** button reverts it to
  `Pending` and, importantly, records that this class should have been asked.
- **Medium/high risk** → a normal approval card. Nothing changes for the work
  that genuinely deserves a person.

The undo window is the safety property: nothing is irreversible for the first
30 minutes, and a human who disagrees teaches the system by disagreeing.

### 2. Preference learning

Every human decision is recorded as a `(features, decision, reason)` sample —
the same features the risk score reads, plus who decided. Once a class of
ticket has enough consistent samples (default 8, all agreeing), the system
adjusts:

- **Consistently approved** → that class drops into the auto-approve lane.
  Announced once: *"Every test-coverage ticket you saw (8/8) you approved — I
  will auto-approve them from now on and post the notice instead. Say `ask
  again: test coverage` to reverse."*
- **Consistently rejected for the same reason** → the system stops *proposing*
  that shape at all. A rejection reason like "no acceptance criteria" becomes a
  pre-flight check: the BA fills the gap before the ticket ever reaches a
  human's inbox.
- **Undo used** → the class moves the other way immediately; one undo outweighs
  several silent approvals, because a person bothering to reverse something is
  the strongest signal available.

Learning is per-project and visible: `#approvals` carries a pinned summary of
what is currently auto-approved and why, and any human can revoke a rule in
plain language.

### 3. Batched, themed approval

What still needs a person is not delivered one card at a time. The SM groups
the pending queue before posting:

> **6 tickets waiting on you**
> ① **Test coverage (4)** — same pattern, low risk → `[Approve all 4]` `[Review]`
> ② **Bug triage (2)** — F031 and F034 overlap ~80%, suggest merging first
>  → `[Merge + approve]` `[Show diff]` `[Keep separate]`

The agent does the reading, grouping and duplicate-detection (what machines are
good at); the human does the judging (what people are good at). Six clicks
become two — and the duplicate that six separate cards would have hidden is
surfaced.

## Configuration

```jsonc
// coxagent.json
{
  "workflow": {
    "human": {
      "gates": { "ready": true, "verify": true },
      "adaptive": {
        "enabled": true,
        "undo_window_minutes": 30,   // 0 disables auto-approval entirely
        "learn_after_samples": 8,    // consistent decisions before a class shifts
        "max_auto_per_cycle": 3      // blast-radius cap, even when confident
      }
    }
  }
}
```

Turning `adaptive.enabled` off returns exactly to the fixed gates of
HYBRID_TEAM.md. Turning gates off returns to full autonomy. The three settings
form the autonomy dial: fixed gate → adaptive gate → no gate.

## Edge cases

- **A wrong auto-approval lands work nobody wanted.** The undo window reverts
  the ticket, and DEV's claim protocol means at most one cycle of work is lost.
  Sensitive paths and `large` complexity never auto-approve regardless of
  history.
- **Learning from a single loud human.** Samples are per-decider; a rule only
  forms from one person's consistent history and is announced with their name,
  so a teammate can see whose taste is encoded.
- **A class drifts.** Any undo, or two rejections in a row, retires the rule
  and returns the class to asking.
- **Nobody is staffed.** With no human in the project, adaptive mode behaves as
  fully autonomous — the announcements still post, so the log stays honest.
- **Batch approval hiding a bad ticket.** Every batch action lists the ids it
  covers and each row stays individually reviewable; "Approve all" is a
  convenience, never a blind bundle.

## Code map

- `crates/application/src/use_cases/approval_risk.rs` — pure risk scoring
- `crates/application/src/use_cases/approval_memory.rs` — preference samples and rules
- `crates/application/src/use_cases/cycle/audits.rs` — auto-approve pass + announcements
- `crates/application/src/config.rs` — `workflow.human.adaptive`
- `crates/presentation/src/server/inbox.rs` — undo endpoint, batch endpoints
- `crates/presentation/src/web/js/inbox.js` — grouped cards, undo affordance

## Related

- [HYBRID_TEAM.md](HYBRID_TEAM.md) — the fixed gates this builds on
- ARCHITECTURE.md — module layout and IO discipline
