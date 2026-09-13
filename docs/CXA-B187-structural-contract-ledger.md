# CXA-B187 — Structural regression contracts for the above-the-fold Overview panels

> Ticket chain: CXA-B181b → CXA-B187 ([1/3] `.1` · **[2/3] `.2` (this ledger)** · [3/3] `.3`).
> Contracts live in `crates/presentation/tests/`; the shared rule set is the
> CXA-B186a harness (`guardrail_scaffold_b201.rs`).
> Batch rule (set by .1, honoured by every batch): a batch may only ADD
> guarded panels; a file leaving the guarded set fails
> `guarded_set_never_shrinks`. One batch owns one test binary — contracts for
> a panel land in the batch that guards it, never retro-edited from a later
> batch.

## Status after CXA-B187.2 (this batch)

| Panel (inventory id) | Source of truth | Contract (test name) | Status |
|---|---|---|---|
| KPI tile strip — Pilot (`#ov-kpis`) | `web/js/kpis.js` | `guardrail_scaffold_b201.rs` (CXA-B186a pilot exemplar) | ✅ done — CXA-B187.1 |
| Alerts strip (`#ov-alerts`) | `web/js/core.js` → `alertsHtml` | `overview/alerts-strip-says-what-and-why` | ✅ done — **CXA-B187.2** |
| Drain banner (`#ov-drain`) | `web/js/shell.js` → `drainBanner` | `overview/drain-banner-names-the-hold-and-the-way-out` | ✅ done — **CXA-B187.2** |
| Activity feed (`#ov-activity`) | `web/js/core.js` → `actItem` | `overview/activity-rows-carry-attribution` | ✅ done — **CXA-B187.2** |
| Working-now strip (`#ov-working`) | `web/js/core.js` → `renderOvWorking` | `overview/working-now-strip-lives-on-two-feeds` | ✅ done — **CXA-B187.2** |
| Changelog + deploy health row (`#ov-health`) | `web/js/chat.js` → `renderHealth` | `overview/health-grid-ships-six-attributed-metrics` | ✅ done — **CXA-B187.2** |
| Diagnostics disclosure | `web/js/core.js` → container-logs block | — | ⬜ pending — **CXA-B187.3** |
| Charts row (`#ov-charts`) | `web/js/chart.js` | — | ⬜ pending — **CXA-B187.3** |
| Attention strip (`#ov-attention`) | `web/js/core.js` → `renderAttention` | — | ⬜ pending — **CXA-B187.3** |

### Guarded files after .2 (may only grow — enforced in-test)

`web/js/core.js` (3 contracts) · `web/js/shell.js` (1) · `web/js/chat.js` (1)

## Harness changes made by .2

**None to the shared scaffold.** `guardrail_scaffold_b201.rs` (the CXA-B186a
harness) is byte-identical to its .1 state — `git diff` is empty for that
file. Batch 2 consumes only its public pattern (ports + pure decision
functions + fail-closed unreadable-file rule + the allowlist-shrink law) and
composes the same building blocks locally in
`panel_structural_contracts_b187_2.rs`, exactly as the harness's own
self-tests invite. No backward-compatibility question arises: nothing the
harness exports changed shape or vanished.

## Negative coverage added by .2 (correctness does not rest on eyeballing)

- `panel_contracts_fail_a_deliberately_malformed_alerts_strip` — renames
  `alertsHtml` and drops the why-copy + why-not branch **in memory** (no
  working-tree edit); the batch must go red, name the drifted contract, list
  every lost anchor and NOT blame the four intact panels.
- `panel_contracts_fail_a_deliberately_malformed_health_grid` — silently
  drops one Team-health metric card (5 `card(` where 6 ship); the count rule
  must catch it and leave the `core.js` panels unblamed.
- Hand-run mutation check recorded for .2 (break → red → revert): renamed
  `function alertsHtml(` → `function renderAlerts(` in `core.js`; the live
  gate failed with exactly `markup lost: \`function alertsHtml(\``; reverted
  with `git checkout`; tree clean. The two in-memory negative tests above
  are the permanent, CI-runnable form of the same proof.

## Missing design data

None — every panel in this batch had a real, contractable structure in the
checked-in sources, so no `ASK SA:` row was needed and nothing was faked.
