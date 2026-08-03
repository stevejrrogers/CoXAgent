# Hybrid teams: humans and agents on one board

**Keywords:** hybrid, human-in-the-loop, approval gates, inbox, assignee, SLA,
mention, PO, BA, SM, QA, review, needs_human_eyes

## Overview

CoXAgent runs an autonomous software team. A hybrid team adds real people —
PO, BA, SM, developers, QA — **without duplicating roles**: every human role
keeps the DECISIONS, its agent counterpart does the DRAFTING and EXECUTION.
The board, the ticket lifecycle, the PR flow and the chat are shared; a human
and an agent are both just actors with different permissions and different
strengths.

The one-line rule: **agents propose and produce, humans approve and own the
exceptions.**

## How it works

### Role split

| Human | Agent counterpart | Boundary |
|---|---|---|
| PO | PO + BA agents | Agents propose features, analyse, write AC → tickets land in `Pending`. Only a human approval moves them to `Ready`. Nothing gets built unapproved (when the `ready` gate is on). |
| BA | BA agent | The agent answers a DEV's question first, from the wiki/docs/history. Questions it cannot ground (policy, verbal customer decisions) are forwarded to the human BA by @mention, with an SLA. |
| SM | SM agent | The agent runs the mechanical ceremonies (standup digest, metrics, retro draft). The human SM reads digests, sets capacity, arbitrates when things stall. |
| Developer | DEV agents | Agents take S/M tickets end to end. LARGE tickets, sensitive paths, and tickets parked after repeated failures route to a human. Humans review every PR held by `needs_human_eyes`. |
| QA | TEST agent | The agent runs regressions and attaches evidence (screenshots, req/resp). The human does exploratory testing and — with the `verify` gate on — is the only one who moves `Fixed → Verified`. |

### The hybrid ticket lifecycle

```
BA-agent drafts → [human: approve Ready] → DEV-agent implements → PR
→ SA-agent reviews → small PRs auto-merge / [human: needs_human_eyes]
→ TEST-agent + evidence → [human: verify] → Done
```

Every `[human: …]` step is an **approval gate** — configurable per project,
so a cautious team turns them all on and a trusting team turns them off one
by one. The gates are the autonomy dial.

### Where a human plugs in (the surfaces)

1. **Inbox ("Waiting for me")** — one view per signed-in user collecting
   everything that waits on THEM: tickets pending approval, PRs held for human
   eyes, evidence awaiting verification, questions addressed to them, tickets
   assigned to them. Every card carries its actions (approve / request changes
   / reject / reply) so the queue is workable in one place.
2. **Chat** — the scrum channel carries the agents' Q&A; a human can answer
   any question in-thread and agents pick the answer up. `@username` in an
   agent question targets a person and lands in their inbox.
3. **Docs** — the wiki is the agents' ground truth. A human BA feeding a
   customer decision into a wiki page (directly, or by telling the assistant)
   is the highest-leverage act in the system: it converts a future
   interruption into a self-served answer.
4. **Review tab** — humans land or reject the PRs the machine holds back.
5. **The assistant** — the project chat agent acts as each person's front
   door: "what needs me today?", "approve F012", "why is PR #14 stuck?".

### Assignment and routing

A ticket's assignee is either an **agent role** (default) or a **username**.
Routing to a human happens automatically when:

- complexity is `large` (architecture-shaping work),
- the diff would touch sensitive paths (same list as `needs_human_eyes`:
  workflows, Dockerfile, compose, scripts/, Cargo.toml, coxagent.json),
- the ticket was parked after `max attempts` agent failures — by definition
  the hardest problems, exactly what a senior human should see.

A human-assigned ticket is skipped by the DEV agents; it appears in that
person's inbox and on the board under their name.

### Questions, mentions and SLAs

The ask protocol (DEV→BA for business, DEV→SA for technical, one forward,
then read the code) gains one hop: **agent → human**. When the answering
agent cannot ground an answer, it posts the question with `@username` (or
`@role` for anyone holding that role). The ticket blocks, the question enters
the person's inbox, and an SLA timer starts. On expiry the question escalates
to the SM channel and joins the impediment digest — a gate must never become
the place tickets go to die.

### Notifications

- In-app: the inbox badge, plus a `#approvals` system channel per project
  where action-carrying cards are posted for the whole team to see.
- Outbound: the existing webhook fans out per-user (Slack/Telegram/…); the
  desktop shell raises native macOS notifications; a per-person digest
  ("waiting on you: 2 approvals, 1 review") posts at end of day.
- Anti-spam: batched every 15 minutes; only high-priority items and expiring
  SLAs ping immediately.

### The learning loop

Human corrections are training signal, captured mechanically:

- A rejected or edited draft carries a reason → recorded into team memory →
  the BA agent drafts differently next sprint.
- A question a human answered twice → the agent proposes a wiki page so the
  third occurrence is self-served.
- A human dev who spots a repeated agent mistake writes a **gate test** (like
  the hexagonal ratchet) or a line in CLAUDE.md — one rule, every future run.

## Configuration

```jsonc
// coxagent.json
{
  "workflow": {
    "human": {
      "gates": { "ready": true, "verify": true },   // merge gate = needs_human_eyes (always on)
      "route_large_to_human": true,
      "question_sla_minutes": 240
    }
  }
}
```

RBAC maps a signed-in user to a team role (PO/BA/SM/DEV/QA). Domain
transitions already enforce which ROLE may perform which move; the mapping
makes that hold for people too: a QA account can Verify but not Ready.

## Adoption path

- **Week 1 — Observer**: all gates on. Agents only draft; humans approve
  everything and learn to read agent output.
- **Weeks 2–3 — Gatekeeper**: merge gate relaxes to `needs_human_eyes` only
  (small PRs auto-land); `ready` and `verify` stay human.
- **Steady state — Mixed pod**: humans keep the ready gate, LARGE tickets and
  exploratory QA. Loosen further using the dashboard's own numbers (PR reject
  rate, escaped bugs).

## Edge cases

- **Human never answers**: the SLA escalates to the SM channel and the
  impediment digest; the SM (human or agent) reassigns or unblocks.
- **Nobody holds the role**: gates for unstaffed roles fall back to any
  admin; an empty team behaves exactly like today's fully-autonomous mode.
- **Agent and human race on one ticket**: the claim protocol already makes
  claims atomic; a human assignment simply removes the ticket from the agent
  candidate pool.
- **Human writes code**: their PR goes through the same SA review and gates
  as an agent PR. One rulebook.

## Code map

- `crates/domain/src/ticket.rs` — assignee, transitions
- `crates/application/src/selection.rs` — agent candidate pools (skip human-assigned)
- `crates/application/src/use_cases/merge_policy.rs` — `needs_human_eyes`, sensitive paths
- `crates/application/src/config.rs` — `workflow.human` gates
- `crates/presentation/src/server/work.rs` — ticket/inbox endpoints
- `crates/presentation/src/web/js/` — inbox view, approval cards

## Related

- ARCHITECTURE.md — module layout, IO discipline
- docs/ (wiki) — per-area pages the agents maintain
