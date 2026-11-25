# Guardrail invariant registry (CXA-B225 / CXA-B181.3)

**Keywords:** guardrail, invariant, registry, allowlist, shrink, regression gate, CXA-B181, CXA-B186a, CXA-B186b, KPI panel, depth contract

## Overview

A shipped UI invariant is only real if something fails when it regresses. This
page is the registry of the invariants the named guardrail suite
(`crates/presentation/tests/guardrail_scaffold_b201.rs`) enforces, what each
one protects, and how the gate is wired so it cannot be silently unhooked.

## The suite

One named, runnable target — `cargo test -p coxagent-presentation --test
guardrail_scaffold_b201` — wired into `.github/workflows/ci.yml`'s
`guard-tests` job (merge-blocking by branch protection). Removing the CI step
turns the wiring guard red (`crates/app/tests/ci_availability_gate.rs` pins
the step payload in `GUARD_TESTS_STEPS`).

## Invariants

| Invariant | Source | Protects | Guarded by |
|---|---|---|---|
| `overview/kpi-tiles-carry-depth` | `web/js/kpis.js` | The five Overview KPI tiles each keep their depth contract: per-day series (14-day window), zero-state hint, click-through — the depth bar CXA-F360 shipped | scaffold self-tests + live scan |

## Shrink-only allowlists

Grandfathered violations live in per-invariant allowlists that may **only
shrink**: a file leaves the list when it is fixed, and never re-enters.
`scaffold::shrink_check` fails any grow. Today the registry has no entries —
the KPI panel obeys its contract.

## Source of truth

The panel inventory (which panels exist, their slots and the pilot
nomination) is `docs/CXA-B204-overview-kpi-panel-inventory.md`; the domain
registry itself is `crates/domain/src/panel.rs` (CXA-B186b). This page only
registers invariants and their enforcement — when a new panel ships an
invariant, add a row here and a check in the scaffold in the same PR.
