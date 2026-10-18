//! CXA-B181.1 — guardrail tests for the above-the-fold DEPLOY CHANGELOG panel
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
    let decided_by: Vec<_> = s
        .reverted_work
        .iter()
        .map(|ev| ev.decided_by.clone())
        .collect();
    let decisions: Vec<_> = s.reverted_work.iter().map(|ev| ev.decision).collect();
    assert_eq!(
        decisions.as_slice(),
        [RevertDecision::Approved],
        "the verdict lands"
    );
    assert_eq!(
        decided_by.last().and_then(|d| d.as_deref()),
        Some("operator"),
        "who"
    );
    assert!(
        s.reverted_work.iter().all(|ev| ev.decided_at.is_some()),
        "when"
    );
    // an already-decided event is never re-decided
    assert!(!s.decide_revert(
        "abc123",
        RevertDecision::Dismissed,
        "someone-else"
    ));
    let decided_by_after: Vec<_> = s
        .reverted_work
        .iter()
        .map(|ev| ev.decided_by.clone())
        .collect();
    assert_eq!(
        decided_by_after.last().and_then(|d| d.as_deref()),
        Some("operator"),
        "the first human verdict sticks"
    );
}

#[test]
fn deploy_history_is_append_only_and_window_scoped() {
    let s = ProjectState {
        tickets: vec![feature()],
        history: vec![deploy_record()],
    };
    assert_eq!(s.history.len(), 1);
    assert_eq!(s.history[0].at, "2026-09-06T10:00:00Z");
    assert_eq!(
        s.history[0].ticket.to_string(),
        "CXA-F001",
        "attribution survives"
    );
}

#[allow(clippy::expect_used)]
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
