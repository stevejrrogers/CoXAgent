//! CXA-B260 regression — the cycle/deploy flow's TERMINAL STATE must be
//! rendered by [`coxagent_presentation::render_report`], the exact function
//! the CLI entry point calls (`crates/app/src/lib.rs`). Before this ticket the
//! report ended after the ticket counts: a failed deploy (and its WHY), a
//! healthy deploy, and even the not-started empty state were all invisible to
//! anyone reading the CLI.
//!
//! Pure functions over the REAL `ProjectState` / `DeployStatus` types — no
//! server, no process spawn, no fabricated fixtures.

use coxagent_application::state::{DeployStatus, ProjectState};
use coxagent_presentation::{
    deploy_terminal_state::{
        deploy_terminal_lines, HINT_NOT_STARTED, LABEL_FAILED, LABEL_NOT_STARTED,
        LABEL_SUCCEEDED,
    },
    render_report,
};

fn failed_attempt(summary: &str) -> DeployStatus {
    DeployStatus {
        at: "2026-09-15T01:07:28Z".to_string(),
        ok: false,
        summary: summary.to_string(),
        commit_sha: Some("ed8fca62".to_string()),
        health_check: None,
        failure_bundle: None,
    }
}

/// AC: a FAILED deploy attempt is attributed in the CLI report — label AND its
/// WHY. Fails on the pre-B260 `render_report`, which emitted no deploy lines.
#[test]
fn render_report_shows_a_failed_deploys_label_and_why() {
    let state = ProjectState {
        deploy: Some(failed_attempt("compose up exited 1")),
        ..ProjectState::default()
    };

    let report = render_report(&state);
    assert!(report.contains(LABEL_FAILED), "missing FAILED label:\n{report}");
    assert!(
        report.contains("why          : compose up exited 1"),
        "failure WHY must survive the full-report path:\n{report}"
    );
    // Attribution of what is live must remain provable in the report too.
    assert!(report.contains("ed8fca62"), "commit sha dropped:\n{report}");
}

/// AC: success names itself in the report rather than blending into silence.
#[test]
fn render_report_shows_a_succeeded_deploy() {
    let state = ProjectState {
        deploy: Some(DeployStatus {
            ok: true,
            summary: "released 0.4.1".to_string(),
            ..failed_attempt("unused")
        }),
        ..ProjectState::default()
    };

    let report = render_report(&state);
    assert!(report.contains(LABEL_SUCCEEDED), "{report}");
    assert!(report.contains("summary      : released 0.4.1"), "{report}");
}

/// AC: before anything runs, the report shows the explicit empty-state hint —
/// never a silently blank terminal section.
#[test]
fn render_report_shows_the_not_started_empty_state() {
    let state = ProjectState::default(); // deploy: None — nothing ran yet

    let report = render_report(&state);
    assert!(report.contains(LABEL_NOT_STARTED), "{report}");
    assert!(report.contains(HINT_NOT_STARTED), "{report}");
}

/// AC: the report path renders EXACTLY what the pure component renders — the
/// wiring cannot drop or reorder lines relative to the tested contract.
#[test]
fn render_report_emits_every_line_the_component_produces() {
    let state = ProjectState {
        deploy: Some(failed_attempt("out of disk")),
        ..ProjectState::default()
    };
    let binding = deploy_terminal_lines(state.deploy.as_ref());
    let expected: Vec<&str> = binding.iter().map(String::as_str).collect();

    let report = render_report(&state);
    for line in expected {
        assert!(report.contains(line), "line `{line}` missing from report:\n{report}");
    }
}
