# Microcopy guardrails (CXA-B191) — copy layer, guards, baseline

**Keywords:** microcopy, copy layer, copy.js, CXA-B164, guardrails, inline
literals, placeholder guard, ellipsis title, toast, empty state, shrink-only,
INLINE_COPY_EXCEPTIONS, COPY_GATE_EXCEPTIONS

## Overview

CXA-B164 found broken/truncated user-facing microcopy across the hub UI
(empty states, toasts, buttons, tooltips/ellipsis cells). CXA-B191 locks the
fix in with three pure regression guards plus a single **copy layer**,
`crates/presentation/src/web/js/copy.js`, served as the first app script
(registered in `APP_JS`, `crates/presentation/src/server/mod.rs`, before
`kpis.js`/`core.js` and every consumer).

## The copy layer

- `window.CXACOPY` — the catalog: one entry per shared string, each with
  `params` (documented `{tokens}`) and a `text` template. Templates carry
  LITERAL `{token}` placeholders; `copyText()` fills them from params.
- `window.copyText(key, params)` — resolves a key; a missing key renders the
  visible `_copy.broken` marker, never a silent empty string.
- `window.toastCopy(key, params, opts)` — toast as a **text node**, announced
  (`role=status`, `aria-live=polite`).
- `window.ellipsisCopy(key, params, where)` — truncated cell that carries the
  **full** string in `title` and `aria-label`.
- `window.skeletonFor(kind, where)` — skeleton span stamped with a fixed
  `data-copy-kind` (CXA-B163 invariant) and a `data-copy-where` surface name.
- `window.__copyIntegrityCheck()` — in-DOM audit used by manual QA.

## The three guards (all pure, no server/network/harness)

| Guard | File | Fails when |
|---|---|---|
| Placeholder guard | `crates/presentation/tests/copy_catalog_b191.rs` | a catalog template uses a token its `params` don't document (or vice versa), a param is dropped/mangled (incl. long strings), or the gate baseline stops being enforced-empty |
| Render contract | `crates/presentation/tests/copy_render_b191.rs` | toast/ellipsis/button/empty-state renderers lose the full text, the accessible `title`/`aria-label`/`role`/`aria-live` attributes, or copy.js stops wiring them |
| Inline-literal gate | `crates/presentation/tests/copy_layer_gate_b191.rs` | presentation JS passes a new user-facing string literal straight to a DOM-writing call (`toast(`, `confirm(`, …) instead of the copy layer, or copy.js is no longer served before its consumers |

Run them with the standard web test command:

```sh
cargo test -p coxagent-presentation --test copy_catalog_b191 \
  --test copy_render_b191 --test copy_layer_gate_b191
```

## Baseline & the shrink-only ratchet

- `copy_layer_gate_b191.rs`: `INLINE_COPY_EXCEPTIONS` starts **EMPTY**. Any
  future entry needs an owner and a linked ticket in its reason, and the list
  may **only shrink** — the test fails on an exception that no longer matches
  a live violation, so dead entries cannot accumulate.
- `copy.js`: `window.COPY_GATE_EXCEPTIONS = []` — same rule, same ratchet.
- Legacy inline literals that predate the gate must go through these lists;
  nothing new may be added without a ticket.

## Deliberate-violation drill (how to verify a guard still bites)

1. **Raw key**: call `copyText("nope.missing")` in a consumer → the render
   contract's broken-marker asserts fail naming the surface.
2. **Placeholder mismatch**: add a `{token}` to a `CXACOPY` template without
   documenting it in `params` → `copy_catalog_b191` / `copy_render_b191` fail
   naming the key.
3. **Inline literal**: add `toast("Saved your changes")` to any presentation
   JS → `copy_layer_gate_b191` fails with file:line and the required fix.
4. **Ellipsis without title**: create a `span.tof` without `title` in copy.js's
   writer (or drop `s.title = full`) → `copy_render_b191` fails on the
   accessible-title contract.
Revert each and confirm `cargo test -p coxagent-presentation` is green.
