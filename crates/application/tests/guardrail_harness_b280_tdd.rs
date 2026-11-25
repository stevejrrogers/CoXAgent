//! TDD tests for CXA-B280 — Guardrail harness scaffold (pure case runner +
//! failure report), the `[1/3]` slice of CXA-B240.
//!
//! Written in the CXA-F237 style: executable acceptance criteria over a pure
//! application-layer module, fixtures built from data the codebase has today.
//! The harness lives in `coxagent_application::guardrails`, following the same
//! pure-gate pattern as `use_cases/run_dev/gates.rs`: every case is a plain
//! function over an input snapshot the caller hands it, so the whole loop runs
//! with zero IO imports anywhere in the module.
//!
//! AC → test map:
//! - AC1 (module exists with case type, runner, report as separate small
//!   files; check functions are pure with zero IO imports):
//!   [`ac1_the_guardrail_unit_is_a_named_described_pure_check`],
//!   [`ac1_module_files_stay_small_and_split_along_cohesive_seams`]
//! - AC2 (runner emits per-case {name, passed, reason} and aggregate counts):
//!   [`ac2_the_runner_emits_per_case_results_with_reasons_and_aggregate_counts`]
//! - AC3 (a failing case prints case name plus expected-vs-actual, never a
//!   bare boolean):
//!   [`ac3_a_failing_case_prints_name_plus_expected_vs_actual_never_a_bare_false`]
//! - AC4 (smoke case green; deliberately broken variant run once to show the
//!   readable failure, then reverted — see [`the_deliberately_broken_smoke_variant_prints_the_full_failure_once`] plus the ticket's captured run below).
//! - AC5 (existing hexagonal gate and full suite stay green — verified by
//!   running `cargo test --workspace` for this change).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::guardrails::{
    run_suite, GuardrailCase, GuardrailReport, GuardrailVerdict, SMOKE_SENTINEL,
};

// ---------------------------------------------------------------------------
// Fixtures — pure cases registered inline, exactly what future CXA-B240
// guardrail cases will look like.
// ---------------------------------------------------------------------------

/// A stand-in "panel inventory" check written the way CXA-B240's real
/// above-the-fold invariant tests will be: one pure closure over a snapshot.
fn panels_complete_case(expected_panels: &[&str]) -> GuardrailCase<Vec<String>> {
    // Owned capture keeps the boxed check `&#39;static`, exactly how a real
    // registry would store its cases.
    let expected: Vec<String> = expected_panels.iter().map(|s| (*s).to_owned()).collect();
    GuardrailCase::new(
        "above_the_fold_panels_complete",
        "every registered above-the-fold panel renders",
        move |rendered: &Vec<String>| {
            let missing: Vec<String> = expected
                .iter()
                .filter(|p| !rendered.iter().any(|r| r == *p))
                .cloned()
                .collect();
            if missing.is_empty() {
                GuardrailVerdict::Hold
            } else {
                GuardrailVerdict::Breach {
                    expected: format!("all of [{}] present in the render set", expected.join(", ")),
                    actual: format!("missing [{}]", missing.join(", ")),
                }
            }
        },
    )
}

// --- AC1: the case unit is identity + intent + a pure check. ---

#[test]
fn ac1_the_guardrail_unit_is_a_named_described_pure_check() {
    let c = panels_complete_case(&["kpi", "board"]);
    assert_eq!(c.name, "above_the_fold_panels_complete");
    assert!(!c.description.is_empty(), "a case states what it defends");

    let verdict = (c.check)(&vec![
        "kpi".to_owned(),
        "board".to_owned(),
        "builds".to_owned(),
    ]);
    assert_eq!(verdict, GuardrailVerdict::Hold);

    // The verdict carries evidence on breach — the raw material of a readable
    // failure line later, straight from the case that detected the break.
    let verdict = (c.check)(&vec!["kpi".to_owned()]);
    match verdict {
        GuardrailVerdict::Breach { expected, actual } => {
            assert!(expected.contains("board"));
            assert_eq!(actual, "missing [board]");
        }
        verdict @ GuardrailVerdict::Hold => {
            panic!("a partial panel list must read as a breach, got {verdict:?}")
        }
    }
}

/// House rule pinned as a guard: one file per cohesive unit, all < 500 lines.
/// Reads the harness source relative to CARGO_MANIFEST_DIR — allowed here,
/// because THIS is the guard doing its job (like the hexagonal gate reading
/// sources); the guardrailed modules themselves stay pure.
#[test]
fn ac1_module_files_stay_small_and_split_along_cohesive_seams() {
    use std::path::PathBuf;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/guardrails");
    for file in ["mod.rs", "case.rs", "runner.rs", "report.rs"] {
        let path = dir.join(file);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("harness file {file} readable: {e}"));
        let lines = src.lines().count();
        assert!(
            lines < 500,
            "{file} is {lines} lines — split along a cohesive seam before growing further"
        );
        // Zero IO imports inside the case / runner units: the runner stays
        // pure even while this (guard) test is permitted to read files.
        if matches!(file, "case.rs" | "runner.rs") {
            for banned in [
                "std::fs",
                "std::process",
                "std::net",
                "reqwest",
                "tokio::",
                "Command::new",
            ] {
                assert!(
                    !src.contains(banned),
                    "{file} must not import IO surface `{banned}` — the runner is pure"
                );
            }
        }
    }
}

// --- AC2: runner results and the aggregate count shape. ---

#[test]
fn ac2_the_runner_emits_per_case_results_with_reasons_and_aggregate_counts() {
    let cases = vec![
        panels_complete_case(&["kpi", "board"]),
        GuardrailCase::new("always_holds", "a trivially true invariant", |_| {
            GuardrailVerdict::Hold
        }),
    ];
    let rendered = vec!["kpi".to_owned(), "board".to_owned()];
    let results = run_suite(&cases, &rendered);

    // Per-case shape: {name, passed, reason}, registration order preserved.
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].name, "above_the_fold_panels_complete");
    assert!(results[0].passed);
    assert!(
        results[0].reason.contains("every registered"),
        "even a PASS says why: {}",
        results[0].reason
    );

    // Aggregate: {passed, failed, total}.
    let report = GuardrailReport::of(&results);
    assert_eq!(
        (report.passed, report.failed, report.total),
        (2, 0, 2),
        "AC2: aggregate counts over the same run"
    );

    // And over a mixed run the arithmetic holds.
    let half = vec![rendered[0].clone()];
    let results = run_suite(&cases, &half);
    let report = GuardrailReport::of(&results);
    assert_eq!((report.passed, report.failed, report.total), (1, 1, 2));
    assert!(!report.all_passed());
}

// --- AC3: failures read as sentences, never as a bare boolean. ---

#[test]
fn ac3_a_failing_case_prints_name_plus_expected_vs_actual_never_a_bare_false() {
    let cases = vec![panels_complete_case(&["kpi", "board", "activity"])];
    let results = run_suite(&cases, &vec!["kpi".to_owned(), "board".to_owned()]);
    let report = GuardrailReport::of(&results);
    let rendered_text = report.render(&results);

    assert!(
        rendered_text.contains("FAIL above_the_fold_panels_complete — "),
        "AC3: the failure line names the case: {rendered_text}"
    );
    assert!(
        rendered_text.contains("expected all of [kpi, board, activity]"),
        "AC3: the EXPECTED side is spelled out: {rendered_text}"
    );
    assert!(
        rendered_text.contains("but got missing [activity]"),
        "AC3: the ACTUAL side is spelled out: {rendered_text}"
    );
    // Counts line up top for CI log scanning.
    assert!(rendered_text.contains("guardrails: 0 passed / 1 failed / 1 total"));
    // Never just `false`.
    assert!(
        !rendered_text.split_whitespace().any(|w| w == "false"),
        "no bare boolean token in the report: {rendered_text}"
    );
}

// --- AC4: smoke case wired end-to-end, plus the deliberate-break demo. ---

#[test]
fn the_smoke_case_runs_green_through_case_runner_report() {
    // The scaffold's registry: exactly one case, the smoke one.
    let cases = vec![smoke_only()];
    let results = run_suite(&cases, &SMOKE_SENTINEL);
    let report = GuardrailReport::of(&results);
    assert_eq!(
        (report.passed, report.failed, report.total),
        (1, 0, 1),
        "AC4: the smoke case passes green through the whole loop"
    );
    assert!(report.render(&results).contains("guardrails: OK"));
}

fn smoke_only() -> GuardrailCase<u32> {
    use coxagent_application::guardrails::smoke_case;
    smoke_case()
}

/// The VERIFY step, made permanent: invert the smoke expectation ONCE by
/// feeding an off-sentinel value and assert the rendered report names the
/// case and shows expected vs actual — then (in the passing direction above)
/// revert to the sentinel. Running both directions in one test proves the
/// failure leg without ever leaving a red suite behind.
#[test]
fn the_deliberately_broken_smoke_variant_prints_the_full_failure_once() {
    // Deliberately broken variant: sentinel flipped by one.
    let broken_input = SMOKE_SENTINEL.wrapping_add(1);
    let results = run_suite(
        &[coxagent_application::guardrails::smoke_case()],
        &broken_input,
    );
    let report = GuardrailReport::of(&results);
    assert_eq!(report.total, 1);
    assert_eq!(report.failed, 1, "the inverted variant must fail");

    let text = report.render(&results);
    assert!(
        text.contains("FAIL harness_smoke_loop_closes"),
        "broken variant names the case: {text}"
    );
    assert!(
        text.contains("expected input == SMOKE_SENTINEL (42), but got 43"),
        "broken variant shows expected vs actual: {text}"
    );
    // …and reverting means: the un-broken run right after is green again.
    let reverted = run_suite(
        &[coxagent_application::guardrails::smoke_case()],
        &SMOKE_SENTINEL,
    );
    assert!(
        GuardrailReport::of(&reverted).all_passed(),
        "revert lands green"
    );
}
