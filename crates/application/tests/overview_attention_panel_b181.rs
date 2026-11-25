//! CXA-B181.1 — guardrail tests for the above-the-fold ATTENTION panel
//! (#ov-attention, fed by `metrics_governance::attention_summary`).
//! Fold set source: `crates/presentation/src/web/index.html`
//! `#view-overview` lines 202-215 ("Above the fold: does the project need me…").
//!
//! Pure only: no server, no network, no DB, no host harness.

use coxagent_application::metrics_governance::{attention_summary, AttentionSummary};
use coxagent_application::state::{InterventionRecord, ProjectState};
use coxagent_domain::kinds::{InterventionKind, TicketType};

fn rec(kind: InterventionKind, area: Option<TicketType>, by: &str, at: &str) -> InterventionRecord {
    InterventionRecord {
        kind,
        ticket: "CXC-B001".to_owned(),
        area,
        by: by.to_owned(),
        at: at.to_owned(),
    }
}

const NOW: &str = "2026-09-10";

#[test]
fn zero_state_renders_the_full_class_grid_not_a_bare_zero() {
    // The zero state is the grid SHAPE: all three areas present, zero-filled
    // with every intervention kind labelled, so the panel never collapses to
    // an empty box a person cannot read.
    let s: AttentionSummary = attention_summary(&ProjectState::default(), NOW);
    for area in [TicketType::Feature, TicketType::Bug, TicketType::Chore] {
        let row = s
            .attention_by_area
            .get(area.key())
            .unwrap_or_else(|| panic!("zero state must keep the {} area", area.key()));
        assert!(!row.is_empty(), "{} grid must stay labelled", area.key());
        assert!(row.values().all(|&c| c == 0));
    }
    assert_eq!(s.interventions_total, 0);
}

#[test]
fn every_intervention_counts_once_in_its_area_and_is_attribution_capable() {
    let st = ProjectState {
        governance_interventions: vec![
            rec(
                InterventionKind::VerifyPass,
                Some(TicketType::Bug),
                "GATE",
                "2026-09-09T10:00:00Z",
            ),
            rec(
                InterventionKind::VerifyPass,
                Some(TicketType::Bug),
                "GATE",
                "2026-09-09T11:00:00Z",
            ),
            rec(
                InterventionKind::ReadyApprove,
                Some(TicketType::Feature),
                "DEV",
                "2026-09-09T12:00:00Z",
            ),
        ],
        ..ProjectState::default()
    };
    let s = attention_summary(&st, NOW);
    let bugs = s.attention_by_area["bug"].values().sum::<u64>();
    let feats = s.attention_by_area["feature"].values().sum::<u64>();
    assert_eq!(bugs, 2, "two bug decisions land in the bug area");
    assert_eq!(feats, 1, "one feature decision lands in the feature area");
    assert_eq!(s.interventions_total, 3, "total counts each decision once");
    assert_eq!(
        s.unattributed, 0,
        "records carrying an area are never re-counted as unattributed"
    );
}

#[test]
fn records_without_a_derivable_area_are_reported_not_silently_dropped() {
    let st = ProjectState {
        governance_interventions: vec![rec(
            InterventionKind::VerifyPass,
            None,
            "GATE",
            "2026-09-09T10:00:00Z",
        )],
        ..ProjectState::default()
    };
    let s = attention_summary(&st, NOW);
    assert_eq!(s.interventions_total, 1, "the decision still exists");
    assert_eq!(
        s.unattributed, 1,
        "unknown area is surfaced explicitly (AC4)"
    );
    assert!(
        s.attention_by_area
            .values()
            .all(|r| r.values().all(|&c| c == 0)),
        "it is never silently assigned to a class"
    );
}
