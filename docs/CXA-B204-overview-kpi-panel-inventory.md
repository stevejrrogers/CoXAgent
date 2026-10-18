# CXA-B204 — Inventory of above-the-fold Overview KPI panels + pilot nomination

Subtask of CXA-B183.1 (pilot-migrate the first Overview panel to the PanelShell
terminal-state contract). **Docs-only ticket: this document is the entire
deliverable. Zero production, test, build-file or dependency edits.**

Tree state when line numbers were taken: worktree of branch `feat/CXA-B202`,
HEAD `bef21bbadd069cb4636d8c095a2207db6b2d711a` (repo map generated
2026-09-10T18:38Z). All `file:line` citations below were read directly in that
tree.

---

## 1. Search method (auditable)

Four passes, in this order:

1. **Layout pass.** Read `crates/presentation/src/web/index.html` and located
   the Overview view. The Overview screen is the default route
   (`shell.js:431` — `if(TITLES[h])nav(h);else nav("overview")`; `core.js:112`
   `nav()` falls back to `"overview"` for unknown hashes). Its DOM is a single
   column of slot divs; the initial-viewport band is `index.html:205–215`
   (`#ov-drain`, `#ov-alerts`, `#ov-working`, `#kpis`, activity/changelog/
   health row, then the collapsed Diagnostics disclosure at `index.html:215`).
2. **Renderer pass.** Grepped every `crates/presentation/src/web/js/*.js`
   (excluding vendored `*.min.js`) for `ov-drain|ov-alerts|ov-working|ov-health|
   ov-activity|ov-changelog|ov-charts|ov-attention|kpis` and for the render
   functions that own them (`renderActive`, `renderOvWorking`, `renderHealth`,
   `alertsHtml`, `chartsHtml`, `renderGovernanceAttention`, `overviewKpis`,
   `drainBanner`). Every `ov-*` id is written from exactly one render function;
   there are no dynamic panel registrations.
3. **Registry pass.** Cross-checked against the merged panel registry
   (CXA-B186b): `crates/domain/src/panel.rs` (slot enum with
   `PanelLayoutSlot::AboveTheFold` at `panel.rs:27`, used by the overview panel
   entry at `panel.rs:39`) and `crates/application/src/use_cases/list_panels.rs`
   (the read that will drive render order). The registry currently declares the
   overview panels; the UI does not yet consume `list_panels` — order below is
   taken from the static `index.html` markup, which is what actually renders.
4. **Contract pass.** Checked what of the CXA-B192 terminal-state contract and
   the CXA-B193 shell is merged: commit `648671fa` (CXA-B192) touched only
   `crates/presentation/src/server/mod.rs` and
   `crates/presentation/src/web/js/copy.js` (loading/empty/error renderers,
   e.g. `copy.js:16` `"loading.generic"`); a `panel_shell.js` exists **only** on
   the unmerged branch `feat/CXA-B193-panel-shell` (tip `b71dc6e3`). Grep for
   `PanelShell|panel_shell|panelShell` across `*.rs`, `*.js`, `*.md` on this
   branch: **0 hits** — nothing above the fold consumes any shell today.

**Count: 7 panels render above the fold on Overview** (drain banner, alerts
strip, working-now strip, KPI tile strip, activity feed, changelog+health row,
and the collapsed Diagnostics disclosure, which is a single `<details>` in the
viewport whose content loads below the fold). Two further Overview panels
(`#ov-charts`, governance attention `#ov-attention`) fall below the fold and are
listed as phase-3 candidates for completeness. Precedent for the "panel stuck on
a bare loading state" defect class: CXA-B131 (work-log panel on `/#activity`),
fixed separately — this inventory is the Overview-side equivalent.

---

## 2. Panel inventory (above the fold)

Shared data source for all panels: the hub state snapshot — `GET /api/state`
pulled by `shell.js:274` and pushed by SSE reconnects, handed to every renderer
through `render(s)` (`chat.js:1745`: `STATE=s; renderSidebar(s); renderActive(); …`).
There is no per-panel fetch, so "loading" below means *snapshot in flight /
fields absent*, and "error" means *fetch or SSE failure, or snapshot present but
the panel's fields absent/malformed*.

| # | Panel (name) | Component file (one path) | Data source / use case | Loading today | Error today | Empty today | PanelShell? | Depth-bar gaps |
|---|--------------|---------------------------|------------------------|---------------|-------------|-------------|-------------|----------------|
| 1 | **KPI tile strip** (`#kpis`) | `crates/presentation/src/web/js/kpis.js` (111 lines; `kpi()` at `kpis.js:14`, tiles wired by `overviewKpis()`) | Derived client-side from the state snapshot by `metricsFrom(s)` (`core.js:160`) — shipped / in-flight / open-bug counts, plus CXA-F360 14-day sparkline + signed delta per tile (`kpis.js:1–2`) | **Bare `0` tiles**: nothing gates on "snapshot not yet arrived"; the first paint renders zero-value tiles (no skeleton; copy-layer `loading.generic` `copy.js:16` unused here) | **Silent zeros**: a failed `/api/state` or missing fields renders as `0` with no distinction from a true zero — no error surface at all | **Has a branch** but it lives outside the file: `core.js:775` early-returns for `!s.tickets&&!s.activity`, and `core.js:781` injects a full-width "panel" placeholder into `#kpis` — no zero-state *hint* naming what fills it | No | Bare 0 with no window (sparkline exists but the headline number still has no today/7d/lifetime selector); silent idle; missing why-copy; zero-state hint is generic |
| 2 | **Activity feed** (`#ov-activity`) | `crates/presentation/src/web/js/core.js` (render branch inside `renderActive`, `core.js:788`) | Snapshot `s.activity` (agent event stream), guarded by the same `core.js:775` early-return | **Nothing** — the div stays empty until the snapshot lands (no skeleton; `loading.inbox` `copy.js:17` unused here) | **Silent idle** — fetch/SSE failure leaves the panel blank with no error copy | **Exists**: `core.js:788` `<div class="empty">activity appears as agents work</div>` — correct message, but shown only on that branch | No | Missing why-copy when stale; no attribution of who/when on the empty state |
| 3 | **Changelog + deploy health row** (`#ov-changelog`, `#ov-health`) | `crates/presentation/src/web/js/core.js` (`core.js:789` innerHTML for changelog; `failureForensicsHtml(dp)` `core.js:746`; `renderHealth(s)` call `core.js:800`) | Snapshot `s.deploy` + container logs (`core.js:740`) for forensics; health via `renderHealth` (`chat.js:1712`, element lookup `chat.js:1713`) | **None** — renders only once the snapshot arrives; blank before | **Silent** — deploy-poll failure renders as absence of the panel, no error copy | Changelog has an empty branch (`core.js:789`); health has none (absent = blank) | No | Missing zero-state hint for health; error states unnamed |
| 4 | **Alerts strip** (`#ov-alerts`) | `crates/presentation/src/web/js/core.js` (`alertsHtml(s,m,spend)` `core.js:166`) | Snapshot tickets/history + spend, reduced into alert chips | **None** — empty div until data arrives | **Silent `[]`** — a failed load is indistinguishable from "no alerts" | "No alerts" renders as an empty strip — no explicit all-clear/zero-state copy | No | Silent idle (no all-clear copy); no attribution of when alerts were last evaluated |
| 5 | **Drain banner** (`#ov-drain`) | `crates/presentation/src/web/js/shell.js` (`drainBanner(elId)` `shell.js:724`) | Drain/pause state from the snapshot (workers draining) | **None** — empty until state arrives | **Silent** — no fetch-error path | Renders nothing when idle (correct: a banner with nothing to say should be absent) | No | Missing why-copy on pause (states *that* draining, not *why*) |
| 6 | **Working-now strip** (`#ov-working`) | `crates/presentation/src/web/js/core.js` (`renderOvWorking()` `core.js:1021`; throttle `OV_WORK_LAST` `core.js:1020`; ground-truth liveness rule `core.js:1040`) | Snapshot workers + liveness array (`window.LIVENESS`, `core.js:1031`) — who is running what right now | **None** — blank until first snapshot | **Silent idle** — a dead SSE socket leaves stale workers listed (CXA-F384-class problem: bad news hidden, `core.js:1000` comment) | Idle case renders "no one working" without saying why (cycle paused? no runner?) | No | Silent idle + missing why-copy (the exact gap the CXA-F384 comment at `core.js:1000` warns about) |
| 7 | **Diagnostics & gates disclosure** (`<details>` at `index.html:215`) | `crates/presentation/src/web/js/core.js` (summary `index.html:215`; CXA-F384 auto-open guard `core.js:1000`; children: drift `js/drift.js`, lesson efficacy `js/lessons.js` — silent `.catch(()=>{window._eff=null;})` `lessons.js:26`, governance attention `renderGovernanceAttention()` `core.js:216` with 60 s cache guard `core.js:209`) | Drift alerts (CXA-F226), lesson-efficacy loop (CXA-F306), governance-attention ledger (CXA-F230) | **None** — collapsed by design, children render lazily | **Silent** — `lessons.js:26` swallows errors; drift/attention fail as absence | Each child has partial empty states, inconsistent | No | Mixed; largest state complexity — defer |

Below-the-fold (phase 3, not counted in the 7): `#ov-charts`
(`chartsHtml(s)` `core.js:243`, wired `core.js:809`), governance attention
`#ov-attention` (`core.js:216`) when expanded.

### PanelShell / contract adoption status (all panels)

* **Consuming PanelShell: 0 of 7.** No `panel_shell.js` exists on this branch —
  only on unmerged `feat/CXA-B193-panel-shell`.
* **Merged and reusable today:** the CXA-B192 pure contract pieces inside
  `copy.js` (loading/empty/error text renderers, `copy.js:16–17`) and the panel
  registry (`crates/domain/src/panel.rs`, `crates/application/src/use_cases/
  list_panels.rs`, `PanelLayoutSlot::AboveTheFold` `panel.rs:27`).
* **Recurring defect class confirmed:** CXA-B131 (work-log panel stuck on bare
  `loading…` with no terminal state) is the same pattern this inventory records
  panel-by-panel; none of the 7 panels has an error terminal state today, and
  5 of 7 have no loading state.

---

## 3. Ranked migration order (lowest cost first)

Cost ≈ lines-to-touch × terminal-state complexity. The three terminal states
per panel are the complexity driver; a panel whose states already exist costs
less than one where all three must be authored.

1. **KPI tile strip** — `kpis.js` (111 lines) + one branch in `core.js`
   `renderActive`. Three real, reachable states (first-paint loading, brand-new
   project empty at `core.js:781`, snapshot-failure error). Smallest surface.
2. **Activity feed** — empty state already exists (`core.js:788`); author
   loading + error; render branch sits in oversized `core.js` (extract, don't
   grow it — 500-line rule).
3. **Alerts strip** — `core.js:166`; small, but "silent `[]`" needs a real
   all-clear + error split (behaviour change, slightly more design than code).
4. **Drain banner** — `shell.js:724`; already stateful, mostly a re-skin onto
   the shell + why-copy.
5. **Changelog + health row** — two elements, two data feeds (`core.js:746`,
   `chat.js:1712`); needs health zero-state first.
6. **Working-now strip** — `core.js:1021`; correctness-sensitive liveness
   ground truth (`core.js:1040`) + throttle; migrate last of the cheap ones.
7. **Diagnostics disclosure** — `core.js:216`, `drift.js`, `lessons.js:26`
   (swallowed catch); three sub-panels with divergent empty/error handling —
   highest complexity, phase 3.

---

## 4. Pilot: **the KPI tile strip (`#kpis`, `crates/presentation/src/web/js/kpis.js`)**

The KPI strip is the smallest above-the-fold panel with a dedicated component
file (111 lines, one render function, one call site) whose three terminal states
are all genuinely reachable today, so a migration exercises loading, error and
empty without fabricating states: *loading* is the first paint before
`/api/state` resolves (currently bare `0` tiles); *empty* is a brand-new project
with no tickets and no activity, which already has a branch — but one that lives
outside the file at `core.js:781` and lacks a zero-state hint naming what fills
the board; *error* is a failed snapshot or SSE drop, which today renders as
silent zeros indistinguishable from a true zero — the exact "bare 0 with no
window / silent idle" depth-bar violation the depth bar exists to kill, on the
most-seen numbers in the product. Blast radius is minimal: `kpis.js` plus one
branch in `core.js:renderActive`, both already touched by the B191 copy-layer
guards, so the existing `copy_render`/`copy_layer_gate` tests give the pilot a
regression harness for free, and the panel registry already declares the
overview strip as `PanelLayoutSlot::AboveTheFold` (`crates/domain/src/panel.rs:27,39`)
— meaning the pilot also becomes the exemplar wiring that proves
`list_panels` + shell + copy-layer work end to end before the remaining six
panels follow in the ranked order above.

---

*Audit trail: search passes described in §1; every `file:line` in §2 was read at
HEAD `bef21bb`. No code, test, build or dependency file was created or modified
by this ticket.*
