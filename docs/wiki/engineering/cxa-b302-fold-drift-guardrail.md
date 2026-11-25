# Above-the-fold inventory freeze + drift guardrail (CXA-B240.2 / B281 / B302)

**Keywords:** above-the-fold, panel inventory, fold inventory, frozen
snapshot, golden file, drift guardrail, dev-gate, re-freeze,
`freeze_fold_inventory`, `above_the_fold_panels.golden`,
`FoldFreezeSourcePort`, `FoldDriftSourcePort`

## Overview

The set of panels rendered **above the fold** on the Overview page is a
frozen contract. The layout (panel slugs in render order) is captured from
the typed presentation manifest into a committed canonical snapshot —

```
crates/presentation/__inventories__/above_the_fold_panels.golden
```

— and every change to it now runs through an active drift guardrail wired
into the dev-gate path (**CXA-B302**, part 3/3).

## How the pieces fit (all hexagonal: IO in adapters, decision pure)

| Piece | Where | Nature |
|---|---|---|
| Canonical entry / parser | `crates/domain/src/panel_inventory_entry.rs` (`PanelInventoryEntry::parse_canonical`) | pure domain |
| Pure comparator | `crates/domain/src/panel_drift.rs` (`panel_inventory_drift`, `render_drift_report`) | pure domain |
| Freeze use case | `crates/application/src/use_cases/freeze_fold_inventory.rs` | pure over port |
| Drift gate runner | `crates/application/src/use_cases/fold_drift_gate.rs` (`run_fold_drift_gate`) | pure over port |
| Ports | `ports/outbound/fold_freeze_source.rs` (manifest rows), `ports/outbound/fold_drift_source.rs` (frozen + live sides) | application ports |
| Adapter | `crates/presentation/src/fold_freeze_adapter.rs` (`ManifestFreezeSource`) | reads the real typed manifest |
| Regenerate binary | `crates/app/src/bin/freeze_fold_inventory.rs` | writes the golden |

## The dev-gate step (B302)

At gate time the runner:

1. loads the committed snapshot **and** recomputes the live inventory — both
   through [`FoldDriftSourcePort`], never touching disk itself;
2. diffs them with the pure comparator;
3. passes with a summary count (`…matches the freeze: N panels at …`) when
   clean, or **fails** printing the rendered report: per-panel findings with
   expected vs actual (e.g. `MISSING  kpis: expected rendered at slot 3, got
   (none)`), the finding count headline, and — crucially — **how to
   re-freeze**.

Tests cover pass / fail-with-report / stale-snapshot guidance paths via
injected fake ports only (no host process): see
`crates/app/tests/fold_drift_guardrail_ci.rs`.

## Re-freeze workflow (when you changed the layout INTENTIONALLY)

```sh
cargo run -p coxagent-app --bin freeze_fold_inventory
```

then commit the regenerated `__inventories__/above_the_fold_panels.golden`.
If the change was not intentional, fix the panel manifest/order instead —
do not regenerate to make a bug disappear.

An empty or unreadable snapshot fails loudly (with the same command
printed); it can never silently pass the gate.
