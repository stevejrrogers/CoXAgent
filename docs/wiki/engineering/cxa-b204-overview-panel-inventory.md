# CXA-B204 — Above-the-fold Overview panel inventory (pilot selection)

**Keywords:** overview, above the fold, panels, terminal states, loading, error, empty, PanelShell, CXA-B192, pilot, depth bar, silent idle, bare zero

## Overview

Audit of every KPI/panel that renders in the initial viewport of the Hub **Overview** view (`/#/` → `#view-overview`), feeding CXA-B183: migrate panels one by one onto the CXA-B192 panel terminal-state contract (skeleton / loaded / empty / error — no bare "loading…", no silent idle).

**Scope fence:** this document only. No production code, tests, build files or dependencies were touched (see Verification).

---

## 1. Search method (auditable)

1. **DOM walk, in paint order.** Read `crates/presentation/src/web/index.html` from `#view-overview` (line 202) down to the closing `</div>` of the view (line 226). The in-source fold cut is explicit: the comment at index.html:203–204 says *"Above the fold … (CXA-F383 declutter — everything diagnostic folds below)"*, and every container after line 205's first four sits above the collapsible `<details class="ov-diag" id="ov-diag">` wrapper at index.html:215, which is collapsed by default and therefore below the fold. **Fold rule used: everything inside `#view-overview` and before `<details id="ov-diag">` counts as above the fold; everything inside the `<details>` counts as below.**
2. **Writer mapping.** For each container id, grepped `crates/presentation/src/web/js/*.js` for `getElementById("<id>")` to find the exact render function and line that paints it, then read each renderer end-to-end for its loading / error / empty behaviour.
3. **Cross-check against existing guardrails.** The fold cut matches the CXA-B181/B181.2 guardrail tests (fold = the panels before the diagnostics disclosure; KPI tiles = `#kpis`, `crates/presentation/src/web/js/kpis.js`), and the B181.1 zero-state tests confirm the metrics the tiles consume.
4. **PanelShell adoption check.** `grep -rn "PanelShell|panel_shell|PanelState|panel_state" crates --include=*.rs` → **0 matches on this branch**. The B192 contract commit (648671fa) exists in history; its pure types are not wired into any renderer yet. The B186b panel registry branch (a15d6d0e, `crates/domain/src/panel.rs` + `list_panels` use case) is **not merged** into this branch. **Adoption today: 0 of 7 panels.**

**Panel count: 7 above-the-fold panels** (the KPI strip is counted as one panel of five tiles; it is one renderer with one data contract). 11 further diagnostic panels live below the fold inside `#ov-diag` and are listed in §4 for auditability only.

## 2. The inventory (DOM order)

Legend — **states**: what actually renders for loading / error / empty, with `file:line` citations. **Depth gaps** use the operator depth bar: bare 0 with no window, silent idle, missing why-copy, missing zero-state hint. **PanelShell**: does the renderer consume the B192 contract?

### P1 — Clean-base drain banner (`#ov-drain`)
- **Component:** `crates/presentation/src/web/js/shell.js` — `drainBanner()` (shell.js:724–745)
- **Data source:** project goal text from the 1 Hz state snapshot (`STATE.refactor_mode` / goal regex, shell.js:723) plus `GET /api/projects/{id}/prs` (shell.js:732) with a module-level cache.
- **Loading:** none — the div is empty until the async `/prs` fetch resolves; the toolbar (its normal chrome) is not painted first. *Silent blank.*
- **Error:** the fetch `catch` at **shell.js:732** renders the plain toolbar and `return`s — **no error copy whatsoever**. Silent idle.
- **Empty:** goal is not a refactor → toolbar, no banner (shell.js:730) — intentional "not applicable", but indistinguishable from the error path above.
- **Depth gaps:** missing why-copy on error; error and empty render identically.
- **PanelShell:** ❌ none.

### P2 — Attention alerts (`#ov-alerts`)
- **Component:** `crates/presentation/src/web/js/core.js` — `alertsHtml()` (core.js:166–186), painted at core.js:796.
- **Data source:** 1 Hz snapshot (`s.deploy`, `s.reverted_work`, `s.tickets` via `metricsFrom`) plus `window._budget` from project config.
- **Loading:** none — blank until the first snapshot render.
- **Error:** none of its own; depends entirely on the global snapshot poll (a dead poll leaves the panel stale-blank; only the separate connection pill notices).
- **Empty:** **core.js:182 `if(!al.length)return "";`** — renders nothing at all ("all clear" is silent); on an empty project the paint site itself writes `''` (core.js:786). "All clear" is indistinguishable from "not loaded / broken" (deliberate declutter, but silent idle by the depth bar's definition).
- **Depth gaps:** missing zero-state hint (no calm all-clear line naming why it's quiet); error state missing entirely.
- **PanelShell:** ❌ none.

### P3 — Working-now strip (`#ov-working`)
- **Component:** `crates/presentation/src/web/js/core.js` — `renderOvWorking()` (core.js:1021–1056), painted at core.js:778.
- **Data source:** its **own** two fetches — `GET /api/projects/{id}/workers` and `GET /api/projects/{id}/agent-liveness` (core.js:1026–1028), 4-second throttle (core.js:1024).
- **Loading:** none — div stays blank until both fetches resolve (throttled repaint, core.js:1024–1025).
- **Error:** **core.js:1055 `.catch(()=>{})`** — fetch failures are swallowed whole; the div remains blank forever. The single worst silent-idle on the page (CXA-B131/B132 class of bug).
- **Empty:** **core.js:1050 `if(!chips.length){setHTML(el,"");return;}`** — renders nothing; "nobody working" is indistinguishable from "fetch failed".
- **Depth gaps:** silent idle on error; missing zero-state hint ("no agents working — start a run / the loop is idle because…"); loading state missing.
- **PanelShell:** ❌ none.

### P4 — Overview KPI tiles (`#kpis`)
- **Component:** `crates/presentation/src/web/js/kpis.js` — `overviewKpis()` (kpis.js:76–100) + `overviewKpiTile` renderer; painted at core.js:798.
- **Data source:** 1 Hz snapshot only — `s.history`, `s.tickets`, `s.spend`, `s.spend_history` (kpis.js:80–88). No extra fetch.
- **Loading:** blank until first snapshot; no skeleton.
- **Error:** none of its own (global poll contract, same as P2).
- **Empty / zero-state:** **best on the page** — every tile carries a why-hint naming what fills it ("ships land here when a ticket reaches documented", kpis.js:89–99) plus a 14-day sparkline and signed delta vs the prior 14 days (kpis.js:1–9). Releases counts distinct versions, not ship events (kpis.js:85–93, the CXA-B171 fix).
- **Depth gaps:** only the loading skeleton is missing; window/trend/zero-hint all present. Bare-0 risk: none — every 0 renders with its hint.
- **PanelShell:** ❌ none.

### P5 — Team health grid (`#ov-health`)
- **Component:** `crates/presentation/src/web/js/chat.js` — `renderHealth()` (chat.js:1712–1744); painted at core.js:800.
- **Data source:** 1 Hz snapshot — `s.tickets`, `s.reviews`, `s.sprint`, `s.decisions`, `s.lessons`.
- **Loading:** blank until first snapshot.
- **Error:** none of its own (global poll contract).
- **Empty:** mixed. Sprint velocity renders a bare **`"—"` with sub "no sprint"** (chat.js:1722) — a bare dash, no why-copy; PR reject rate renders **`"—"`** when no reviews exist (chat.js:1740) with only `0✓ / 0✗` as sub. Open bugs / WIP / refactor debt render bare counts (`0`, chat.js:1738–1741) with static subs.
- **Depth gaps:** **bare 0 / bare — with no window or trend** on velocity, reject rate, WIP, bugs; zero-state hints missing on four of six cards. Numbers here have no 14-day window (contrast P4).
- **PanelShell:** ❌ none.

### P6 — Recent activity (`#ov-activity`)
- **Component:** `crates/presentation/src/web/js/core.js:814` (inline map over `s.activity`, items via `actItem` at core.js:589).
- **Data source:** 1 Hz snapshot `s.activity` (last 7 entries, reversed).
- **Loading:** blank until first snapshot.
- **Error:** none of its own.
- **Empty:** **core.js:814** `'<div class="empty">no activity yet</div>'` (and the pre-first-snapshot variant `activity appears as agents work`, core.js:788). Compliant copy, but no attribution of *why* it's empty (loop not started vs nothing shipped yet).
- **Depth gaps:** loading skeleton; why-copy on empty.
- **PanelShell:** ❌ none.

### P7 — Releases changelog (`#ov-changelog`)
- **Component:** `crates/presentation/src/web/js/core.js:816` (inline map over `s.history`).
- **Data source:** 1 Hz snapshot `s.history` (last 6 versions, reversed).
- **Loading:** blank until first snapshot.
- **Error:** none of its own.
- **Empty:** **core.js:816** `'<div class="empty">no releases yet</div>'` (and the pre-first-snapshot variant, core.js:789).
- **Depth gaps:** loading skeleton; why-copy on empty.
- **PanelShell:** ❌ none.

## 3. Ranked migration order (lowest cost first)

Cost = lines-to-touch × state complexity (number of distinct terminal states the renderer must grow, weighted by whether they need new fetch/error plumbing):

| Rank | Panel | Lines to touch | State complexity | Why this rank |
|---|---|---|---|---|
| 1 | P6 Recent activity | 1 (core.js:814) | low — empty exists; loading+error come free from the shared shell + connection signal | Single expression; empty copy already compliant |
| 2 | P7 Releases | 1 (core.js:816) | low — same shape as P6 | Same |
| 3 | P3 Working-now strip | 2 (core.js:1050, 1055) | med — own fetches; swallowed `catch` must become a real error state | Two silent-idle lines, self-contained |
| 4 | P1 Drain banner | 3 (shell.js:730, 732) | med — async fetch + cache; error path currently disguised as empty | Small but async |
| 5 | P2 Attention alerts | 2 (core.js:182 + paint site 786/796) | med-high — needs a product call: is "all clear" a rendered state or intentional silence? | Behavioural decision gates the code |
| 6 | P4 KPI tiles | ~2 (kpis.js:76 + paint site core.js:798) | low risk, larger surface — 5 tiles, only skeleton missing | Depth bar already satisfied |
| 7 | P5 Team health | ~8 (chat.js:1712–1744) | high — four cards need window/trend/why-copy redesign | Biggest copy + layout surface |

## 4. Below the fold (audit trail, out of scope)

Inside the collapsed `<details id="ov-diag">` (index.html:215–226), auto-opens on bad news via `ovDiagAutoOpen()` (core.js:1004–1016): Drift alerts (`#ov-drift`, drift.js), Go-live readiness (`#ov-preflight`, mcp.js:209–233 — has its own ok/warn/blocked chips and a "No line items reported." empty), Project goal (`#ov-goal`, shell.js:119–133 — has a compliant "No goal set yet — add one…" empty), Velocity (`#ov-velocity`, core.js:187–199), **Cycle performance (`#ov-cycle-perf` — DEAD: no JS writes this id anywhere; permanent silent blank, flag for the follow-up ticket)**, Governance attention (`#ov-attention`, core.js:215–241 — renders nothing on zero gates by design), Lesson efficacy (`#ov-lessons`, lessons.js — renders nothing with zero lessons), Deploy status (`#ov-deploy`, core.js:798–805 — renders nothing when no deploy exists), Charts (`#ov-charts`, core.js:243+), Design system (`#ov-design`, core.js:360+). Plus the Team row (activity/releases above) nothing else.

## 5. Pilot panel — exactly one: **P3 Working-now strip (`#ov-working`)**

**Justification.** The pilot must be the smallest above-the-fold panel whose migration exercises **all three** terminal states, and Working-now is the only one where all three are independently real today without new plumbing: it owns its own two data fetches (`/workers`, `/agent-liveness`, core.js:1026–1028), so **loading** (blank-until-fetch at core.js:1021–1032), **error** (the swallowed `.catch(()=>{})` at core.js:1055 — a genuine, reachable failure mode with a distinct cause: network/hub down) and **empty** (no chips: nobody working, core.js:1050) are three *different* runtime paths, not three flavours of the same snapshot. Its blast radius is the smallest of any panel with an own-error path: one self-contained 36-line renderer that writes exactly one div (`#ov-working`), has no other caller, consumes no shared snapshot, and breaks nothing else if its contract changes — P6/P7 are cheaper in raw lines but their *error* state is entangled with the global 1 Hz poll (plumbing connection state into them is a bigger, riskier change than their line count suggests), and P4/P5 are far larger surfaces. It is also the panel where the fix pays off most per line: today a hub outage leaves the front door of the Overview silently claiming nothing is happening — precisely the CXA-B131/B132 class of "silent idle" bug this epic exists to kill. Migrating it lands the B192 contract's Skeleton → Loaded / Empty / Error transition end-to-end and produces the reference implementation every other panel then copies.

## Verification

- `git diff --stat` after this change shows **only** `docs/wiki/engineering/cxa-b204-overview-panel-inventory.md` (new file) — zero production, test, or build-file edits.
- Doc exists at exactly one path: `docs/wiki/engineering/cxa-b204-overview-panel-inventory.md`.
