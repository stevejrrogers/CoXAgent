//! Per-terminal-state presentation tests for the cycle/deploy flow
//! (CXA-B260). Pure functions over the REAL `DeployStatus` domain type — no
//! process spawn, no server, no fixtures beyond what `state/ops.rs` defines.

use super::*;
use coxagent_application::state::HealthCheckResult;

fn attempt(ok: bool, summary: &str) -> DeployStatus {
    DeployStatus {
        at: "2026-09-15T01:07:28Z".to_string(),
        ok,
        summary: summary.to_string(),
        commit_sha: Some("ed8fca62".to_string()),
        health_check: None,
        failure_bundle: None,
    }
}

fn probe(passed: bool, http_status: Option<u16>) -> HealthCheckResult {
    HealthCheckResult {
        passed,
        http_status,
        response_time_ms: Some(12),
    }
}

#[test]
fn a_not_yet_terminal_flow_renders_the_explicit_empty_state_hint() {
    let lines = deploy_terminal_lines(None);
    let joined = lines.join("\n");
    assert!(joined.contains(LABEL_NOT_STARTED), "{joined}");
    assert!(joined.contains(HINT_NOT_STARTED), "{joined}");
    // A blank terminal area is exactly what this state must prevent.
    assert!(!joined.trim().is_empty());
}

#[test]
fn a_succeeded_deploy_renders_its_label_and_facts() {
    let lines = deploy_terminal_lines(Some(&attempt(true, "released 0.4.1")));
    let joined = lines.join("\n");
    assert!(joined.contains(LABEL_SUCCEEDED), "{joined}");
    assert!(joined.contains("2026-09-15T01:07:28Z"), "{joined}");
    assert!(joined.contains("ed8fca62"), "{joined}");
    assert!(joined.contains("summary      : released 0.4.1"), "{joined}");
}

#[test]
fn a_failed_deploy_always_renders_its_why() {
    let lines = deploy_terminal_lines(Some(&attempt(false, "compose up exited 1")));
    let joined = lines.join("\n");
    assert!(joined.contains(LABEL_FAILED), "{joined}");
    assert!(
        joined.contains("why          : compose up exited 1"),
        "{joined}"
    );
}

#[test]
fn a_failed_deploy_with_a_blank_reason_still_never_silently_hides_the_why() {
    let mut bare = attempt(false, "");
    bare.summary = "   ".to_string();
    let lines = deploy_terminal_lines(Some(&bare));
    let joined = lines.join("\n");
    assert!(joined.contains(LABEL_FAILED), "{joined}");
    assert!(
        joined.contains("(no reason recorded)"),
        "failure must name its empty WHY explicitly: {joined}"
    );
}

#[test]
fn a_failed_health_probe_is_attributed_on_top_of_the_failure() {
    let mut failed = attempt(false, "gateway unreachable after deploy");
    failed.health_check = Some(probe(false, Some(503)));
    let joined = deploy_terminal_lines(Some(&failed)).join("\n");
    assert!(
        joined.contains("health       : failed (HTTP 503)"),
        "{joined}"
    );
}

#[test]
fn a_passing_health_probe_is_attributed_on_success() {
    let mut healthy = attempt(true, "released 0.4.1");
    healthy.health_check = Some(probe(true, Some(200)));
    let joined = deploy_terminal_lines(Some(&healthy)).join("\n");
    assert!(joined.contains("health       : passed"), "{joined}");
}

#[test]
fn every_state_classifies_to_a_nonempty_label() {
    // Exhaustive-name check: each classifiable snapshot yields a label; new
    // variants cannot appear without touching `classify`.
    let snapshots: Vec<Option<DeployStatus>> = vec![
        None,
        Some(attempt(true, "ok")),
        Some(attempt(false, "boom")),
    ];
    for deploy in &snapshots {
        assert!(!DeployTerminal::classify(deploy.as_ref()).label().is_empty());
    }
    assert_eq!(DeployTerminal::NotStarted.label(), LABEL_NOT_STARTED);
}

#[test]
fn every_failure_reason_slot_is_explicitly_named() {
    // The WHY line is key-pinned so screen-scrapers (and humans) can rely on it.
    let joined = deploy_terminal_lines(Some(&attempt(false, "out of disk"))).join("\n");
    assert!(joined.contains("why          : out of disk"), "{joined}");
}
