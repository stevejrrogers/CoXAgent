# CXA-B218 — UI depth contract, registry and depth gate (CXA-B188a/b/c) — handoff

**Keywords:** depth bar, depth contract, depth registry, depth gate, ratchet,
grandfather list, shrink-only, CXA-B188a, CXA-B188b, CXA-B188c, hexagonal_gate,
copy_layer_gate, evaluate, DepthElementKind

## Overview

The house depth bar says no shipped UI surface may be shallow: a LIST ships
with filter/sort/counts/pagination/empty-state, a NUMBER with window/delta/
zero-state hint, a STATE with attribution and an off-reason, an ACTION with an
inverse, a confirm naming the blast radius, and a dimmed (never hidden)
permission where unauthorized. This page is the handoff for the three slices
that lock the bar in mechanically, in the same pattern as the hexagonal gate
(`cxa-b211`) and the microcopy guardrails (`cxa-b191`): pure decision, pinned
data file, gate test, shrink-only ratchet.

## The three slices and where they live

| Slice | What | File |
|---|---|---|
| CXA-B188a — contract model | The PURE checker: what each element kind REQUIRES and how violations are reported. No IO, no repo knowledge. | `crates/domain/src/depth.rs` (`DepthElementKind`, `DepthElement`, `DepthViolation`, `evaluate`) |
| CXA-B188b — registry + gate | The ONE place a shipped element declares its kind and the depth elements it carries; the gate test that enforces the bar. | `crates/presentation/src/depth_registry.rs` (data), `crates/app/tests/depth_gate.rs` (gate) |
| CXA-B188c — this handoff | The doc you are reading: contract rules, gate usage, grandfather status. | `docs/wiki/engineering/cxa-b218-depth-contract-gates.md` |

## The contract rules (what the checker enforces)

- `DepthElementKind::required()` is the single definition of the bar per kind —
  list: `filter, sort, counts, pagination, empty_state`; number:
  `window, delta, zero_state_hint`; state: `attribution, off_reason`; action:
  `inverse, confirm, permission_dimmed`.
- A registry may only CLAIM capabilities its kind requires
  (`DepthElementKind::accepts`); a typo must never look like compliance, and
  the registry unit tests fail on an unknown capability or a duplicate /
  non-namespaced id.
- The decision is `evaluate(&[DepthElement]) -> Vec<DepthViolation>`: input
  order, deterministic, one violation per non-compliant element, message
  naming element, kind and exactly what is missing. Compliant elements produce
  nothing.

## Gate usage

Run the gate the way CI does (same family as the other guard tests):

```sh
cargo test -p coxagent-app --test depth_gate
cargo test -p coxagent-domain --lib depth          # checker unit tests
cargo test -p coxagent-presentation --lib depth_registry
```

Adding a UI surface: declare it in `depth_registry()` with its kind and every
required depth element. Ship it deep or the gate fails with the element id and
the missing list. The gate pins the registry file on disk
(`pub fn depth_registry()` must remain the declaration point), so deleting or
renaming the registry fails the run instead of silently unguarding the bar.

## Grandfather status (the ratchet)

- The gate's `GRANDFATHERED` list holds pre-depth-bar elements with a reason
  each. Today exactly one entry: `alerts.history` (CXA-F235 webhook spool —
  has newest-first sort and an empty state, no filter/pagination yet).
- THE LIST MAY ONLY SHRINK. Deepen an element → remove its entry in the same
  PR. Never add an entry: new elements must ship deep. The gate also fails if
  an entry stops matching a live violation, so dead entries cannot accumulate.
- There is no second escape hatch: the failure message points at the list only
  to say where it is NOT to be used.

## Deliberate-violation drill (verify the gate still bites)

1. Strip `overview.kpis` of `has` in the registry → `depth_gate` fails naming
   `overview.kpis` and `window, delta, zero_state_hint`.
2. Claim a capability the kind does not require (e.g. `filter` on a number) →
   `depth_registry` unit tests fail on the illegal claim.
3. Remove the `alerts.history` grandfather entry without deepening it →
   `depth_gate` fails naming the element.
Revert each and confirm the three commands above are green.
