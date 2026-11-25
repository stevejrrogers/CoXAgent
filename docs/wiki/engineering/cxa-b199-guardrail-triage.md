# Guardrail triage — above-the-fold Overview panels (CXA-B199.3)

Final closeout of the CXA-B181 → B195 → B199 guardrail lineage. Every cell
below is a **named, build-enforced assertion** (a `#[test]` in one of the
guardrail suites wired into CI by the `guard-tests` job — see
`crates/app/tests/ci_availability_gate.rs::GUARDRAIL_GATE_STEPS` and the
`Overview panel guardrails (CXA-B199.3 regression gate)` step in
`.github/workflows/ci.yml`). A violated cell fails the build; there are no
expected-failures, no ignores, and no grandfathered files in this gate.

Invariant families:

- **terminal** — the four common states (skeleton / ready / empty / error)
  are rendered from the shared snapshot contract
  (`overview_panel_terminal_state_b194.rs`, `overview_terminal_state_b196.rs`,
  per-panel suites).
- **attribution** — an off/paused/failing/stale state names WHO, WHEN and WHY
  in the view model and the rendered output (asserted in the per-panel
  suites).
- **depth** — numeric panels carry a window label (today/7d/lifetime), a
  trend/delta where the VM has history, and a zero-state hint where it does
  not (`guardrail_scaffold_b201.rs` exemplar + per-panel suites).
- **wire** — the suite is registered in the named CI gate; dropping the step
  fails `ci_availability_gate` (`GUARDRAIL_GATE_STEPS`).

## Per-panel × per-invariant matrix

| Panel (`#id`, source) | terminal | attribution | depth | wire |
|---|---|---|---|---|
| KPI tiles (`#kpis`, `web/js/kpis.js`) | green | green | green | green |
| Activity timeline (`#ov-activity`, `actItem()`) | green | green | green | green |
| Alerts strip (`#ov-alerts`, `alertsHtml()`) | green | green | green | green |
| Deploy health (`#ov-deploy`, forensics/rollback sub-renders) | green | green | green | green |
| Drain banner (`#ov-drain`, `drainBanner()`) | green | green | green | green |
| Working strip (`#ov-working`, `renderOvWorking()`) | green | green | green | green |
| Attention panel (`#ov-attention`, `metrics_governance::attention_summary`) | green | green | green | green |
| Deploy changelog (`#ov-changelog`, `state.history`/`DeployRecord`) | green | green | green | green |

Every row is green: all four families hold for all eight above-the-fold
panels (the B228 enumeration is the exhaustive list; the matrix covers it).

## B199.1 / B199.2 expected-failure closeout

None survived. The B199.1 (application) and B199.2 (presentation) suites in
this landing compile, run and pass as-is — the F326 fail-closed test-env
guard (merged on main after those subtasks were parked) made their
"expected failure" mode (silent skip when the snapshot fixture env is
absent) impossible: a missing env now fails the run instead of skipping it,
so every expected-fail either became a plain green assertion or was
expressible without the env at all. No defect refs were needed.

## Production one-line fixes made along the way

- `crates/application/tests/overview_fold_guardrails_b181.rs`: `Ticket::set_technical_design`
  signature drift after the CXA-B224 Config/schema landing — the test now
  constructs the typed `TechnicalDesign` struct (with `..Default::default()`)
  instead of the old bare `String` (1-line-equivalent mechanical fix).

## Verification log (this ticket)

1. Full guardrail suite + gate run locally: `cargo test -p coxagent-presentation
   --test guardrail_scaffold_b201 --test overview_*_b199` and
   `cargo test -p coxagent-application --test overview_*_b181`, plus
   `cargo test -p coxagent-app --test ci_availability_gate` — all green.
2. Gate-bites proof: removed the `Overview panel guardrails` step from the
   `wired_workflow` fixture — `ci_availability_gate` failed with
   "…no longer runs the CXA-B199.3 guardrail gate step…" — then reverted.
3. Skip audit: `grep -rn '#\[ignore\]\|expected-fail\|DEFECT-'` over the
   guardrail test tree — zero hits; no skipped test carries no defect ref.

## CXA-B242c — triage closeout [3/3] (evidence per open item)

Each open B199.3 item was confirmed against the actual code and closed with
one-line evidence below. No item required escalation; nothing new invented.

| # | Open item | Status | Evidence |
|---|---|---|---|
| 1 | Guardrail gate wired into CI (`guard-tests` runs the named Overview suites) | CLOSED | `.github/workflows/ci.yml` step `Overview panel guardrails (CXA-B199.3 regression gate)`; wiring drop-proof via `ci_availability_gate::GUARDRAIL_GATE_STEPS` → `test result: ok. 26 passed` |
| 2 | Six presentation per-panel suites pass | CLOSED | `cargo test -p coxagent-presentation --test overview_{kpis,activity,alerts,deploy_health,drain,working_strip}_panel_b199` → `25 passed; 0 failed` |
| 3 | Application fold + panel suites pass (incl. `set_technical_design` typed-signature fix noted above) | CLOSED | `cargo test -p coxagent-application --test overview_fold_guardrails_b181 --test overview_attention_panel_b181 --test overview_deploy_panel_b181` → all green |
| 4 | Scaffold exemplar self-tests (`guardrail_scaffold_b201.rs`) | CLOSED | `cargo test -p coxagent-presentation --test guardrail_scaffold_b201` → `10 passed; 0 failed` |
| 5 | Attribution invariants as PURE domain checks (B242a / CXA-B273) | CLOSED | `crates/application/src/attribution_invariants.rs`; `cargo test -p coxagent-application --lib attribution_tests` → `18 passed; 0 failed` |

No missing regression test surfaced during closeout — every matrix cell maps
to a named `#[test]` above, so none needed to be added beyond this record.

### Recorded full verification run (B242c) — GREEN

```
cargo test -p coxagent-presentation --test guardrail_scaffold_b201 \
  --test overview_kpis_panel_b199 --test overview_activity_panel_b199 \
  --test overview_alerts_panel_b199 --test overview_deploy_health_panel_b199 \
  --test overview_drain_panel_b199 --test overview_working_strip_b199 && \
cargo test -p coxagent-app --test ci_availability_gate && \
cargo test -p coxagent-application --lib attribution_tests
```
Summary lines:
- per-panel guardrails: `test result: ok. 25 passed; 0 failed` (+ scaffold `ok. 10 passed`)
- harxes-style availability/guardrail gate: `test result: ok. 26 passed; 0 failed`
- attribution invariants: `test result: ok. 18 passed; 0 failed`

**Triage state after closeout: zero open B199.3 items.**

## Series summary — CXA-B242

- **B242a** (`dc990bd8`, #754): attribution invariants as pure domain checks
  over recorded state-change data (who/when/why) — `attribution_invariants.rs`,
  18 pure tests, zero IO.
- **B242b**: gate wiring — named Overview-panel suites enforced by the CI
  `guard-tests` job, guarded by `GUARDRAIL_GATE_STEPS`.
- **B242c** (this ticket): closeout only — items verified & closed with the
  evidence table above, one full green run recorded. Closeout diff touches
  only this triage/closeout record.
