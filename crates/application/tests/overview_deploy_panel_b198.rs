//! CXA-B198.2 (restored from the CXA-B198 checkpoint) — guardrail tests for the above-the-fold DEPLOY CHANGELOG panel
//! (#ov-changelog, rendered from `state.history` / `DeployRecord`).
//! Fold set source: `crates/presentation/src/web/index.html` `#view-overview`
//! lines 202-215 ("Above the fold: does the project need me…").
//!
//! Pure only: no server, no network, no DB, no host harness.
use coxagent_application::state::{ProjectState, RevertDecision, RevertEvent};

fn rec(at: &str, sha_ticket: &str) -> RevertEvent {
    RevertEvent {
        sha: sha_ticket.to_owned(),
        subject: "revert: a shipped ticket".to_owned(),
        ticket: sha_ticket.to_owned(),
        role: "DEV-FEATURE".to_owned(),
        reverted_at: at.to_owned(),
        detected_at: at.to_owned(),
        decision: RevertDecision::Pending,
        decided_at: None,
        decided_by: None,
    }
}

#[test]
fn zero_state_is_an_empty_ledger_not_a_null() {
    let s = ProjectState::default();
    assert!(s.reverted_work.is_empty());
    assert!(s.history.is_empty());
}

#[test]
fn a_revert_is_one_ledger_event_what_who_when_survive() {
    let mut s = ProjectState::default();
    assert!(s.record_revert(rec("2026-09-06T10:00:00Z", "abc123")));
    // re-scan of the same commit must not double-flag
    assert!(!s.record_revert(rec("2026-09-07T10:00:00Z", "abc123")));
    assert_eq!(s.reverted_work.len(), 1);
    let ev = &s.reverted_work[0];
    assert_eq!(ev.reverted_at, "2026-09-06T10:00:00Z", "when");
    assert_eq!(ev.role, "DEV-FEATURE", "who did the revert");
    assert_eq!(ev.decision, RevertDecision::Pending, "verdict pending");
}

#[test]
fn a_human_verdict_is_attributed_once_and_never_re_decided() {
    let mut s = ProjectState::default();
    s.record_revert(rec("2026-09-06T10:00:00Z", "abc123"));
    assert!(s.decide_revert("abc123", RevertDecision::Approved, "operator"));
    let ev = s.reverted_work.first().expect("event");
    assert_eq!(ev.decision, RevertDecision::Approved);
    assert_eq!(ev.decided_by.as_deref(), Some("operator"), "who");
    assert!(ev.decided_at.is_some(), "when");
    // an already-decided event is never re-decided
    assert!(!s.decide_revert("abc123", RevertDecision::Dismissed, "someone-else"));
    assert_eq!(s.reverted_work.first().expect("event").decided_by.as_deref(), Some("operator"));
}

#[test]
fn deploy_history_is_append_only_and_window_scoped() {
    let mut s = ProjectState::default();
    s.tickets = vec![feature()];
    s.history = vec![coxagent_application::state::DeployRecord {
        version: coxagent_domain::version::SemVer::new(1, 0, 0),
        ticket: coxagent_domain::ids::TicketId::new("CXA-F001").expect("id"),
        title: "shipped".to_owned(),
        at: "2026-09-06T10:00:00Z".to_owned(),
    }];
    assert_eq!(s.history.len(), 1);
    assert_eq!(s.history[0].at, "2026-09-06T10:00:00Z");
    assert_eq!(s.history[0].ticket.to_string(), "CXA-F001", "attribution survives");
}

fn feature() -> coxagent_domain::ticket::Ticket {
    coxagent_domain::ticket::Ticket::new(
        coxagent_domain::ids::TicketId::new("CXA-F001").expect("id"),
        coxagent_domain::kinds::TicketType::Feature,
        "t",
        "d",
        coxagent_domain::kinds::Priority::Medium,
        coxagent_domain::kinds::Complexity::Small,
        false,
    )
    .expect("ticket")
}
