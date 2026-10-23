//! Pure tests for [`crate::attribution_invariants`] (CXA-B199.3 slice 1).
//!
//! Fixtures are built ONLY from real state types/constructors
//! (`ProjectState::default()`, record struct literals) — no IO, no server,
//! no fabricated harness.

use crate::attribution_invariants::*;
use crate::state::{
    ActivityEntry, EngineIncident, InterventionRecord, ProjectState, RevertDecision, RevertEvent,
    WorkspaceRun,
};

/// Fixture from real constructors only.
fn snapshot(
    activity: Vec<ActivityEntry>,
    interventions: Vec<InterventionRecord>,
    incidents: Vec<EngineIncident>,
    reverted_work: Vec<RevertEvent>,
    workspace_run: Option<WorkspaceRun>,
) -> ProjectState {
    ProjectState {
        activity,
        governance_interventions: interventions,
        engine_incidents: incidents,
        reverted_work,
        workspace_run: workspace_run.unwrap_or_default(),
        ..ProjectState::default()
    }
}

/// InterventionRecord has no `Default`; build a real one from its actual fields.
fn approved_by(by: &str) -> InterventionRecord {
    InterventionRecord {
        kind: coxagent_domain::kinds::InterventionKind::ReadyApprove,
        ticket: String::new(),
        area: None,
        by: by.to_owned(),
        at: EARLIER.into(),
    }
}

const NOW: &str = "2026-09-15T12:00:00Z";
const EARLIER: &str = "2026-09-15T11:30:00Z";
const LATER_THAN_NOW: &str = "2026-09-15T12:05:00Z";

/// Real RevertEvent fixture from its actual fields (`ops.rs`).
#[allow(clippy::too_many_arguments)]
fn revert_event(
    role: &str,
    reverted_at: &str,
    detected_at: &str,
    decision: RevertDecision,
    decided_by: Option<&str>,
    decided_at: Option<&str>,
) -> RevertEvent {
    let decided_by = decided_by.map(str::to_owned);
    let decided_at = decided_at.map(str::to_owned);
    RevertEvent {
        sha: "abc1234".into(),
        subject: "revert: feat(CXA-B000): sample".into(),
        ticket: "CXA-B000".into(),
        role: role.into(),
        reverted_at: reverted_at.into(),
        detected_at: detected_at.into(),
        decision,
        decided_at,
        decided_by,
    }
}

// ---------- accept path ----------

#[test]
fn a_snapshot_where_every_change_carries_attribution_is_clean() {
    let s = snapshot(
        vec![ActivityEntry {
            at: EARLIER.into(),
            agent: "DEV".into(),
            action: "started implementing CXA-B273".into(),
            ticket: None,
        }],
        vec![approved_by("human-operator")],
        vec![],
        vec![revert_event(
            "QA-REVIEW",
            EARLIER,
            NOW,
            RevertDecision::Pending,
            None,
            None,
        )],
        Some(WorkspaceRun {
            running: false,
            by: "human-admin".into(),
            at: EARLIER.into(),
            reason: "draining before deploy".into(),
        }),
    );
    assert_eq!(
        audit_project_attribution(&s, NOW),
        Vec::new(),
        "accept path must produce zero violations"
    );
}

#[test]
fn a_never_touched_workspace_switch_has_nothing_to_attribute() {
    let s = snapshot(vec![], vec![], vec![], vec![], None);
    assert_eq!(audit_project_attribution(&s, NOW), Vec::new());
}

// ---------- missing actor / missing timestamp ----------

#[test]
fn an_activity_row_without_an_actor_violates_carries_attribution() {
    let s = snapshot(
        vec![ActivityEntry {
            at: EARLIER.into(),
            agent: String::new(),
            action: "started implementing".into(),
            ticket: None,
        }],
        vec![],
        vec![],
        vec![],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, RULE_CARRIES_ATTRIBUTION);
    assert_eq!(v[0].field, "agent");
    assert_eq!(v[0].location, "activity[0]");
}

#[test]
fn an_activity_row_without_a_timestamp_violates_carries_attribution() {
    let s = snapshot(
        vec![ActivityEntry {
            at: String::new(),
            agent: "DEV".into(),
            action: "started implementing".into(),
            ticket: None,
        }],
        vec![],
        vec![],
        vec![],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].field, "at");
    assert_eq!(v[0].rule, RULE_CARRIES_ATTRIBUTION);
}

#[test]
fn an_intervention_without_a_decision_maker_is_flagged() {
    let s = snapshot(vec![], vec![approved_by("")], vec![], vec![], None);
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, RULE_CARRIES_ATTRIBUTION);
    assert_eq!(v[0].field, "by");
    assert_eq!(v[0].location, "governance_interventions[0]");
}

#[test]
fn a_blank_whitespace_actor_counts_as_missing_not_present() {
    let s = snapshot(
        vec![ActivityEntry {
            at: EARLIER.into(),
            agent: "   ".into(),
            action: "started implementing".into(),
            ticket: None,
        }],
        vec![],
        vec![],
        vec![],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].field, "agent");
}

// ---------- empty reason ----------

#[test]
fn an_engine_incident_with_an_empty_reason_violates_reason_non_empty() {
    let s = snapshot(
        vec![],
        vec![],
        vec![EngineIncident {
            engine: "claude".into(),
            reason: String::new(),
            role: "DEV".into(),
            since: EARLIER.into(),
            hits: 1,
        }],
        vec![],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, RULE_REASON_NON_EMPTY);
    assert_eq!(v[0].field, "reason");
    assert_eq!(v[0].location, "engine_incidents[0]");
    assert_eq!(v[0].found, "<empty>");
    assert_eq!(v[0].expected, "a non-empty value");
}

#[test]
fn an_activity_action_that_is_the_why_may_not_be_blank() {
    let s = snapshot(
        vec![ActivityEntry {
            at: EARLIER.into(),
            agent: "DEV".into(),
            action: "  ".into(),
            ticket: None,
        }],
        vec![],
        vec![],
        vec![],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, RULE_REASON_NON_EMPTY);
    assert_eq!(v[0].field, "action");
}

#[test]
fn a_recorded_flip_must_carry_the_admin_and_stamp_but_the_reason_stays_optional() {
    let flip = WorkspaceRun {
        running: true,
        by: "human-admin".into(),
        at: EARLIER.into(),
        reason: String::new(), // deliberate bare drain-pause — legal
    };
    let s = snapshot(vec![], vec![], vec![], vec![], Some(flip));
    assert_eq!(audit_project_attribution(&s, NOW), Vec::new());
}

#[test]
fn a_flip_with_a_timestamp_but_no_admin_is_half_attributed_and_flagged() {
    let flip = WorkspaceRun {
        running: false,
        by: String::new(), // half-attributed: stamp present, actor absent
        at: EARLIER.into(),
        reason: String::new(),
    };
    let s = snapshot(vec![], vec![], vec![], vec![], Some(flip));
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, RULE_CARRIES_ATTRIBUTION);
    assert_eq!(v[0].location, "workspace_run");
    assert_eq!(v[0].field, "by");
}

// ---------- unparseable timestamps ----------

#[test]
fn an_unparseable_reverted_at_yields_the_parse_rule_with_found_vs_expected() {
    let s = snapshot(
        vec![],
        vec![],
        vec![],
        vec![revert_event(
            "QA-REVIEW",
            "yesterday",
            NOW,
            RevertDecision::Pending,
            None,
            None,
        )],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, RULE_TIMESTAMP_PARSEABLE);
    assert_eq!(v[0].field, "reverted_at");
    assert_eq!(v[0].found, "\"yesterday\"");
    assert_eq!(v[0].expected, "an RFC3339 timestamp");
}

// ---------- misordered timestamps ----------

#[test]
fn a_detection_before_the_revert_it_detected_violates_timestamp_order() {
    let s = snapshot(
        vec![],
        vec![],
        vec![],
        vec![revert_event(
            "QA-REVIEW",
            NOW,
            EARLIER,
            RevertDecision::Pending,
            None,
            None,
        )],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, RULE_TIMESTAMP_ORDER);
    assert_eq!(v[0].field, "detected_at");
    assert!(v[0].found.contains("before reverted"), "{}", v[0].found);
}

#[test]
fn a_decided_verdict_without_a_decision_maker_violates_carries_attribution() {
    let s = snapshot(
        vec![],
        vec![],
        vec![],
        vec![revert_event(
            "QA-REVIEW",
            EARLIER,
            NOW,
            RevertDecision::Approved,
            None,
            Some(NOW),
        )],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert!(
        v.iter()
            .any(|x| x.rule == RULE_CARRIES_ATTRIBUTION && x.field == "decided_by"),
        "{v:?}"
    );
}

#[test]
fn a_decision_before_the_commit_it_decides_violates_timestamp_order_on_decided_at() {
    let s = snapshot(
        vec![],
        vec![],
        vec![],
        vec![revert_event(
            "QA-REVIEW",
            NOW,
            NOW,
            RevertDecision::Approved,
            Some("human-operator"),
            Some(EARLIER),
        )],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, RULE_TIMESTAMP_ORDER);
    assert_eq!(v[0].field, "decided_at");
}

// ---------- timestamps in the future ----------

#[test]
fn a_stamp_after_the_reference_now_violates_timestamp_not_future() {
    let s = snapshot(
        vec![ActivityEntry {
            at: LATER_THAN_NOW.into(),
            agent: "DEV".into(),
            action: "claimed a ticket before it happened".into(),
            ticket: None,
        }],
        vec![],
        vec![],
        vec![],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, RULE_TIMESTAMP_NOT_FUTURE);
    assert_eq!(v[0].field, "at");
}

#[test]
fn an_exact_now_stamp_is_not_treated_as_future() {
    let s = snapshot(
        vec![ActivityEntry {
            at: NOW.into(),
            agent: "DEV".into(),
            action: "recorded exactly now".into(),
            ticket: None,
        }],
        vec![],
        vec![],
        vec![],
        None,
    );
    assert_eq!(audit_project_attribution(&s, NOW), Vec::new());
}

// ---------- structured result quality ----------

#[test]
fn every_violation_displays_as_a_self_describing_sentence() {
    let s = snapshot(
        vec![ActivityEntry {
            at: String::new(),
            agent: String::new(),
            action: String::new(),
            ticket: None,
        }],
        vec![],
        vec![],
        vec![],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert!(v.len() >= 2);
    for violation in &v {
        let line = violation.to_string();
        assert!(line.starts_with('['), "{line}");
        assert!(line.contains(violation.location.as_str()), "{line}");
        assert!(line.contains(violation.field), "{line}");
        assert!(
            line.contains("expected") && line.contains("found"),
            "{line}"
        );
    }
}

#[test]
fn all_three_activity_failures_on_one_row_are_reported_plural_not_first_only() {
    let s = snapshot(
        vec![ActivityEntry {
            at: String::new(),
            agent: String::new(),
            action: String::new(),
            ticket: None,
        }],
        vec![],
        vec![],
        vec![],
        None,
    );
    let v = audit_project_attribution(&s, NOW);
    assert_eq!(v.len(), 3, "{v:?}");
    let mut fields: Vec<_> = v.iter().map(|x| x.field).collect();
    fields.sort_unstable();
    assert_eq!(fields, vec!["action", "agent", "at"]);
}
