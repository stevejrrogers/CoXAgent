# CXA-B238.1 — Middle-group invariant tests: panel inventory + scope contract

Handoff note for subtasks 2/3 and 3/3 of CXA-B238 ("Invariant tests for
above-the-fold Overview panels, middle group"). Read this before deriving any
panel invariants — do not re-derive the enumeration.

## Source of truth for "middle group"

The frozen manifest `OVERVIEW_PANELS` (`crates/presentation/src/overview_panels.rs`,
frozen by CXA-B195/CXA-B228, pinned by `tests/overview_panel_manifest_b195.rs`)
lists all seven above-the-fold panels in DOM order:

| # | id (`OverviewPanelId`) | dom_id      | group   |
|---|------------------------|-------------|---------|
| 1 | Drain                  | `ov-drain`  | group 1 (header band, run-state owned by WorkspaceRun adapter) |
| 2 | Alerts                 | `ov-alerts` | **group 2 — middle** |
| 3 | Working                | `ov-working`| **group 2 — middle** |
| 4 | Kpis                   | `kpis`      | **group 2 — middle** |
| 5 | Health                 | `ov-health` | **group 2 — middle** |
| 6 | RecentActivity         | `ov-activity`| group 3 (feed tail below the fold line inside the same screen; asserted in CXA-B198.2's own slot) |
| 7 | Releases               | `ov-changelog`| group 3 |

CX A-B238 targets the **middle group = panels 2–5 exactly**: Alerts, Working,
Kpis, Health. Panel count reconciles with the design note for groups 2/3:
four middle panels here, two tail feed panels there (RecentActivity,
Releases), one header panel already covered by the B181 lineage (Drain).
Panels 1, 6, 7 are OUT OF SCOPE for CXA-B238.

Harness rule (inherited from CXA-B198.1): pure Rust tests over real domain /
application types only. No server, no network port, no Node harness, no new
dependencies. Fixtures build `coxagent_application::state::ProjectState`
values and read them through existing pure projections
(`metrics::compute`, state accessors).

## Per-panel invariant contracts (3–5 each)

### 2. Alerts (`ov-alerts`, ALERT_STATES = Loading/Ready/Attention)
Data source: 1 Hz `/state` snapshot fields — deploy failure flag, budget cap,
open-bug pile-up, reverted work awaiting review (`core.js alertsHtml()`).
1. **Membership/order**: alert severity is total and ordered — critical
   (deploy failed / budget capped) sorts strictly before advisory (reverted
   work, open bugs); equal-severity entries keep insertion (snapshot field)
   order.
2. **Content mapping**: every alert derives from a real snapshot field of
   `ProjectState` — an alert exists iff its trigger condition holds on the
   state (no fabricated or heuristic-only alerts); zero triggers ⇒ empty list.
3. **Defined empty/zero state**: healthy default state yields the named
   quiet state (no panic, no bare `0` row); quiet ≠ error.
4. **Counts/totals**: the reported open-bug count equals `bugs_open` from
   `metrics::compute` — one definition of "open bug", never a second count.
5. **Attribution**: each critical alert carries when (timestamp/source event)
   and why (its trigger), so the UI can say WHY it is off/failing.

### 3. Working (`ov-working`, WORKING_STATES = Empty/Ready)
Data source: activity trail + per-agent worker status (`renderOvWorking()`).
1. **Membership**: the strip lists exactly the agents whose current state is
   busy-on-a-ticket — idle/blocked/errored workers are excluded, present
   workers appear once (unique by agent name).
2. **Order**: deterministic presentation order (activity recency, newest
   first), independent of HashMap iteration.
3. **Content mapping**: each row maps 1:1 onto a live worker record
   (agent role + ticket alias); a stale/dead worker must not render as
   working.
4. **Empty/zero state**: no busy workers ⇒ defined Empty terminal state (the
   breathing hint), never a skeleton forever, never an error.
5. **Attribution/drill-down**: every row names who (agent), what (ticket),
   where (log link target).

### 4. KPI tiles (`kpis`, KPI_STATES = Loading/Zero/Ready)
Data source: client-side day bucketing over state history: activity, tickets,
releases, cost (`kpis.js overviewKpis()`). Pure layer: `Metrics`.
1. **Membership**: the tile set is fixed and complete —
   tickets/shipped/bugs/cost (CXA-F360); a Metrics projection always feeds
   all four, none fabricated from nothing.
2. **Window discipline**: trend/delta windows (today / 14-day sparkline) never
   fabricate days that shipped nothing (`deploys_by_day` only has UTC days
   actually present in deploy history, oldest-first).
3. **Zero-state semantics**: empty project ⇒ every tile reads its NAMED Zero
   state (hint text), counts stay 0 without exceeding their populations
   (`features_shipped ≤ total_tickets`; ratio within 0–100).
4. **Determinism / totals**: `metrics::compute(&state)` is pure (same input ⇒
   equal output) and window sums reconcile to lifetime totals exactly —
   no double counting across window boundaries.
5. **Unit integrity**: money stays money (cost tiles sum spend fields in USD,
   no unit drift between today/14d/lifetime buckets).

### 5. Team health cards (`ov-health`, KPI_STATES = Loading/Zero/Ready)
Data source: state snapshot tickets/reviews/sprint commitments
(`chat.js renderHealth()`).
1. **Membership**: card set fixed by the health view contract — velocity, WIP,
   unfixed bugs, PR reject rate; each card sourced from one canonical field.
2. **WIP definition**: in-flight counts use exactly one population
   (InProgress + Ready ⇒ `features_in_flight + bugs pending fix`),
   disjoint from shipped/open so cards never double-count a ticket.
3. **Ratio sanity**: PR reject-rate and ratio cards are percentages in
   0..=100 even with zero denominators (defined 0/quiet case, not NaN/div0).
4. **Zero/empty state**: default `ProjectState` ⇒ every card renders its
   named zero state with a fill-in hint, never a bare 0 and never Error.
5. **Determinism**: card values derive purely from the snapshot (equal states
   ⇒ equal cards); drill-down ids (board/review tabs) survive into the data.

## Status after 1/3

- [x] Build env repaired (`cargo clean -p tree-sitter`, cache warmed once).
- [x] Inventory written (this file).
- [x] Harness scaffolded: `overview_middle_group_invariants_b238.rs`
      (pure-Rust fixture helpers shared by all four panels' invariant tests).
- [x] Golden exemplar panel implemented and green under scoped filter:
      **KPI tiles** (`overview_middle_group_kpis::*`) — its five invariants
      above each have a passing test exercising the pattern for 2/3–3/3.
- [ ] Alerts invariants (subtask 2/3)
- [ ] Working invariants (subtask 2/3)
- [ ] Health invariants (subtask 3/3)

## Status after 3/3 — CXA-B269a middle group CLOSED (CXA-B322)

- [x] Alerts invariants (2/3)
- [x] Working invariants (2/3)
- [x] Health invariants (3/3): WIP disjointness, PR reject-rate percentage
      sanity with zero denominators, attention-bucket exhaustiveness.
- [x] Final-third + depth sweep (3/3 / CXA-B322):
      `reverted_work_alert_action_carries_its_guardrail_decision_path` — the
      destructive revert adjudication tested WITH its guardrail (nothing
      pending may be decided; a verdict never silently overwritten) and its
      attribution stamps (who decided_by / when decided_at);
      `reverted_alert_consequence_only_fires_for_pending_reviews` — zero-state
      inverse of the needs-review consequence;
      permission model exercised via the real `can_review()` gate that drives
      the `can_act` flag on reverted-work inbox rows (dimmed-not-hidden).
- [x] No TODO / #[ignore] / skipped tests left in the middle-group scope.
- [x] 17 middle-group invariant tests green locally; hexagonal gate green.
- [x] Ledger cap ratchet landed (closeout): `revert_ledger_prunes_oldest_first_past_the_max_cap`
      pins MAX_REVERT_EVENTS=100 FIFO eviction (was documented-only before) and
      `record_revert_dedupes_by_sha_even_when_redetected_with_a_new_decision` pins the
      CXA-F047 one-sha-one-event dedupe with no silent overwrite.
- [x] Dimmed-not-hidden pinned at the predicate level:
      `revert_adjudication_permission_is_dimmed_not_hidden_per_role_tier` walks every
      AuthRole tier against the exact `can_review()` gate that serializes as `can_act`
      on reverted-work inbox rows (`presentation/src/server/inbox.rs`) — Viewer/BA dimmed,
      gate owners enabled. Outer panels cannot inherit this today: the shipped tree has no
      PanelShell abstraction (fresh-inventory prerequisite flagged to SA — see PR notes).

### Follow-ups for the outer groups

1. Group 3 feed panels (RecentActivity `ov-activity`, Releases
   `ov-changelog`) still need their own invariant pass under a successor
   ticket — same harness pattern applies; pagination assertions (>~50-row
   feeds) were not exercisable in-scope because these tests assert the pure
   projections, not the DOM row window.
2. ~~Ledger cap edge documented but not ratchet-tested~~ DONE in CXA-B322:
   `revert_ledger_prunes_oldest_first_past_the_max_cap` + sha-dedupe test now pin it;
   if MAX_REVERT_EVENTS ever changes, the pinned 100 fails loudly — bump consciously.
