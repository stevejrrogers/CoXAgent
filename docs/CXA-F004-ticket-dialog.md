FOLDER: Engineering

# Ticket Dialog (New + Read)

**Keywords:** new ticket, create ticket, showTicket, openNewTicket, saveTicket, teamAnalyze, ticket-refine, ticket detail overlay, ov-newticket, ov-ticket

## Overview

CXA-F004 is the two ticket dialogs in the web dashboard: **new-ticket** (file an idea into the backlog) and **ticket-read** (full detail of one ticket). It serves anyone who files or reviews work: a user proposes an idea here and reads every agent-run outcome — design specs, test cases, attachments, comments — by clicking a ticket anywhere in the UI. It is presentation only; tickets are written through REST endpoints backed by `crates/presentation/src/server/work.rs`.

## How it works

Both dialogs are classic-script overlays defined as static HTML `<div class="ov">` modals in `index.html`, driven by functions in `shell.js` (creation) and `chat.js` (detail).

1. **Open new-ticket.** Any view calls global [`openNewTicket()`](crates/presentation/src/web/js/shell.js):250 from an inline `onclick`. It resets every field (`nt-title`, rich editor `nt-desc`, selects `nt-type`/`nt-prio`/`nt-cx`, checkbox `nt-ui`, acceptance textarea `nt-ac`, error line), hides any prior team notes (`nt-notes`), then adds `.open` to overlay `#ov-newticket`.
2. **(Optional) team refine.** [`teamAnalyze()`](crates/presentation/src/web/js/shell.js):220 POSTs the idea (`{description}`) to `/api/projects/:pid/ticket-refine`. This runs [`RefineTicketUseCase`](crates/presentation/src/server/work.rs):178 so BA/PO/SA/PD propose improvements; on success it backfills title / description / priority / complexity / has_ui / acceptance_criteria into the form and renders per-role notes using [`TK_ROLE_COLOR`](crates/presentation/src/web/js/shell.js):208.
3. **Save.** [`saveTicket(startFlow)`](crates/presentation/src/web/js/shell.js):251 requires a non-empty title then builds a payload from all fields (see Interface), POSTs `/api/projects/:pid/tickets`. With "Save & Start Flow" it also POSTs `/control/resume`. Either way it closes the dialog with a toast (`toast(...)`), refetches `/state`, re-renders via [`render(s)`](crates/presentation/src/web/js/core.js), and refreshes runner state.
4. **Open read dialog.** Everywhere a ticket id renders it carries an inline click handler calling global [`showTicket(id)`](crates/presentation/src/web/js/chat.js):1815 — home cards at [home.js](crates/presentation/src/web/js/home.js):184/:213/:277 and inbox rows at [inbox.js](crates/presentation/src/web/js/inbox.js):29.
5. **Load detail.** [`showTicket()`](crates/presentation/src/web/js/chat.js):1815 opens overlay `#ov-ticket`, shows "loading…", then GETs `/api/projects/:pid/ticket/:id`. Detail-only fields absent from list payloads arrive here; if fetch fails or returns nothing it falls back to whatever row exists in cached global state.
6. **Render + act.** It injects HTML into container div `ticket-body`: header with id · type; status; editable priority pills calling [`setPriority(id,p)`](crates/presentation/src/web/js/mcp.js):119 → `/ticket/:id/priority`; assignee controls via [`assignTicket()`](crates/presentation/src/web/js/chat.js):1904 → `/ticket/:id/assign`; blocked-by/blocks dependency chips ([depChips()]:1808); markdown + mermaid description (`mdRender`, renderMermaidIn); acceptance criteria list; test cases with evidence screenshots ([`:1843])([`:1850]); technical & UI specs from design; attachments via `/ticket/:id/attachments?name=` ([uploadAttachment()]:1885); cost-hold banner gated by approval ([approveCost()]:1915 → `/approve-cost`). The action row depends on status: Edit ([editTicket()]:1916 → `/edit`), Work-on-next ([workNext()]:1934 → sets priority high then resumes), Approve→Ready / Mark Verified / Send-back via [humanGate()]:1909 → one of `/ready`,`verify`,`send-back`,`unpark`, Reject ([rejectTicket()] → `/reject`) when pending/open.

Below sits a comments thread loaded by [renderTicketComments(tid)]:1939 from GET `/comments?ticket=` and posted by [postTicketComment(tid)]:1969.

All writes are optimistic REST calls followed by calling `showTicket(id)` again or relying on SSE snapshot refresh.

## Usage

Create a ticket:

```
openNewTicket()
```

Fill Idea/title (required); optionally type Details then click *Let the team analyze & refine* to have BA · PO · SA · PD draft improvements. Set Type / Priority / Size / Has UI / Acceptance criteria as needed. Click *Save* (`saveTicket(false)`) or *Save & Start Flow* (`saveTicket(true)`).

Equivalent REST request:

```
POST /api/projects/<pid>/tickets
Content-Type: application/json

{
  "title": "Add password reset",
  "description": "...",
  "type": "feature",
  "priority": "medium",
  "complexity": "medium",
  "has_ui": false,
  "acceptance_criteria": [
    "User can request an email reset link",
    "Reset link expires after 15 minutes"
  ]
}
```

Read one ticket's full record:

```
GET /api/projects/<pid>/ticket/<TICKET-ID>
-> { id:"CXA-F004", type:"feature", status:"pending", title:"...",
     description:"...", priority:"medium", complexity:"medium",
     has_ui:false,
     design:{ technical:{ approach:"...", files:[...], api_contract:"...",
              test_plan:"..." }, ux:{ user_flow:"...", screens:[...] } },
     acceptance_criteria:[ "...","..." ] }
```

## Interface

Modal element ids referenced across scripts:

| id | kind | purpose |
|----|------|---------|
| #ov-newticket | overlay | create dialog wrapper |
| #ov-ticket | overlay | read dialog wrapper |
| #ticket-body | container div inside #ov-ticket | injected detail HTML lives here |
| #nt-title | input | idea/title (required) |
| #nt-desc | contenteditable rich editor | details body |
| #nt-type / #nt-prio / #nt-cx / #nt-ui / #nt-ac | selects + checkbox + textarea | form fields |

Global JS functions used across views:

```
openNewTicket()
teamAnalyze()
saveTicket(startFlow)
showTicket(id)
editTicket(id)
saveTicketEdit(id)
workNext(id)
humanGate(id,'ready'|'verify'|'send-back'|'unpark')
rejectTicket(id)
approveCost(id)
assignTicket(id,'')            // '' returns to agents pool
setPriority(id,'high'|'medium'|'low')
uploadAttachment(t.id,inputEl)
renderMermaidIn(scopeEl)?      // optional helper when present
```

Project-scoped endpoints this feature consumes (registered in server/mod.rs):

```
POST   tickets                    create backlog item
GET    ticket/{id}                full detail record
POST   ticket/{id}/priority       set priority pill
POST   ticket/{id}/reject         close/reject a pending or open item
POST   ticket/{id}/ready          human approve -> Ready
POST   ticket/{id}/verify         mark verified
POST   ticket/{id}/send-back      reviewer sends back toward agents
POST   ticket/{id}/unpark         resume an unparked item
POST   ticket/{id}/assign         assign to a person or return to pool
POST   ticket/{id}/attachments    upload an attachment (raw bytes body, name in query)
POST   ticket/{id}/approve-cost   approve cost hold so agents may run it

GET    comments?ticket={id}       load the thread for one ticket (chat.js:1939)

Out-of-band: POST /api/projects/<pid>/ticket-refine is the team analyze step,
handled by RefineTicketUseCase in work.rs:178.
```

There is no CLI surface for either dialog.

## Configuration

No dedicated configuration flags drive CXA-F004; behaviour comes from hard-coded markup in `index.html`:

- Select option lists are static: `#nt-type` = feature/bug/chore, `#nt-prio` = high/medium/low (default `medium` selected), `#nt-cx` = small/medium/large (default `medium` selected).
- Engine-backed refinement (`teamAnalyze`) requires a configured engine; it fails with a message "needs a configured engine" when none is available.

## Edge cases and limits

This feature deliberately does NOT cover:

- **No validation on the server for required fields.** `saveTicket` enforces a non-empty title client-side only; empty descriptions and zero acceptance criteria are accepted.
- **Read fallback is silent.** If `GET /ticket/:id` fails or returns no id, `showTicket` falls back to the cached list row; if that is missing too it renders "ticket not found".
- **No delete.** The dialogs offer Reject for pending/open items but no general delete action.
- **25 MB attachment cap** enforced client-side in `uploadAttachment` (chat.js:1887).
- **Auto-refresh only via SSE snapshot.** A dialog left open does not live-update by itself until a state tick lands; writes re-invoke `showTicket(id)` to reflect immediately.

## Code map

The real files implementing this feature:

crates/presentation/src/web/index.html - static markup for both dialogs: overlay wrappers #ov-newticket (~line 688) and #ov-ticket + #ticket-body (~line 679), all form fields (#nt-title/#nt-desc/#nt-type/#nt-prio/#nt-cx/#nt-ui/#nt-ac), refine button, Save / Save & Start Flow actions.

crates/presentation/src/web/js/shell.js - creation side: openNewTicket():250, saveTicket():251 (POST /tickets), teamAnalyze():220 (POST /ticket-refine), TK_ROLE_COLOR at :208.

crates/presentation/src/web/js/chat.js - read side: showTicket():1815 builds the full detail view in #ticket-body; editTicket()/saveTicketEdit():1916/:1928; workNext():1934; renderTicketComments(tid):1939; postTicketComment(tid):1969; uploadAttachment():1885; approveCost(); humanGate().

crates/presentation/src/web/js/mcp.js - setPriority(id,p):119 (POST /ticket/:id/priority).

crates/presentation/src/server/work.rs - HTTP handlers: create_ticket(:208), ticket_detail_ep(:49), ticket_refine(:178); registration of these and sibling routes lives in server/mod.rs (:772 onwards).

crates/presentation/src/server/mod.rs - axum router wiring for every ticket route this feature calls (:806/:807 etc.).

e2e/specs/tickets.spec.ts - the one Playwright spec covering the new-ticket dialog (opens via openNewTicket(), asserts all fields render, cancels cleanly with no console errors). There is no separate read-dialog spec nor any golden screenshot snapshots for these dialogs today.

## Related

CXA-F004 touches presentation only; its data contract maps to backend tickets handled in crates/domain and crates/infrastructure. See PLAN.md "3k Data" for the minimal anti-Jira ticket model. The dashboard SPA of which these dialogs are part must pass cd e2e && npx playwright test before merge.
