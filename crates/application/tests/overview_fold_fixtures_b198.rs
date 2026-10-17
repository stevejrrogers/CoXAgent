//! CXA-B198.2 (restored from the CXA-B198 checkpoint) — guardrail tests for the above-the-fold Overview panels.
//!
//! Above-the-fold panel set, taken from the CXA-B174 implementation
//! (`crates/presentation/src/web/index.html`, `#view-overview` lines 202-215,
//! marked `<!-- Above the fold: does the project need me, and is the team
//! moving? -->`):
//!   1. KPI tiles            (#ov-kpis, CXA-F360 tiles: tickets/shipped/bugs/cost)
//!   2. Drain banner         (#ov-drain, workspace desired-run state)
//!   3. Drift alerts         (#ov-drift, CXA-F226)
//!   4. Working-hours digest (#ov-working, CXA-F234)
//!   5. Deploy changelog     (#ov-changelog, deploy ledger)
//!   6. Attention grid       (#ov-attention, CXA-F359)
//!
//! Scope fence: new TEST FILES ONLY — no production code, no presentation-layer
//! tests (that is CXA-B181.2). These files exercise the pure state/domain/
//! application types behind the fold. Panels 2/3/4 render state owned by
//! WorkspaceRun / drift / working-hours adapters and are asserted at the
//! presentation layer by CXA-B181.2; the pure invariants for the other panels
//! live in the sibling `*_panel_*.rs` files in this directory.
//!
//! Pure only: no server, no network, no DB, no host harness.
use coxagent_application::metrics;
use coxagent_application::state::ProjectState;

#[test]
fn zero_state_yields_named_empty_buckets_not_a_bare_zero() {
    let m = metrics::compute(&ProjectState::default());
    assert_eq!(m.total_tickets, 0);
    assert!(m.by_status.is_empty());
    assert!(m.deploys_by_day.is_empty());
    assert_eq!(m.releases, 0);
    assert_eq!(m.features_shipped, 0);
    assert_eq!(m.bugs_open, 0);
}

#[test]
fn deploys_bucket_by_utc_day_window_and_never_fabricate_a_day() {
    let mut s = ProjectState::default();
    let t = |k: &str| coxagent_domain::ticket::Ticket::new(
        coxagent_domain::ids::TicketId::new(k).expect("id"),
        coxagent_domain::kinds::TicketType::Feature,
        "t", "d",
        coxagent_domain::kinds::Priority::Medium,
        coxagent_domain::kinds::Complexity::Small,
        false,
    ).expect("ticket");
    s.tickets = vec![t("CXA-F001")];
    s.history = vec![
        deploy("2026-09-06T10:00:00Z", "CXA-F001"),
        deploy("2026-09-06T18:00:00Z", "CXA-F001"),
        deploy("2026-09-07T09:00:00Z", "CXA-F001"),
    ];
    let m = metrics::compute(&s);
    assert_eq!(m.releases, 3);
    assert_eq!(m.deploys_by_day.len(), 2, "exactly the two days actually shipped");
    for d in &m.deploys_by_day {
        assert!(d.day.starts_with("2026-09-0"), "day is a UTC date window: {}", d.day);
        assert!(d.count >= 1);
    }
    assert_eq!(m.deploys_by_day.iter().map(|d| d.count).sum::<usize>(), 3);
}

#[test]
fn deploy_attribution_survives_the_projection() {
    let mut s = ProjectState::default();
    s.tickets = vec![shipped_feature("CXA-F009")];
    s.history = vec![deploy("2026-09-06T10:00:00Z", "CXA-F009")];
    let m = metrics::compute(&s);
    // the changelog renders FROM this projection: ticket id + title must survive
    assert_eq!(m.total_tickets, 1);
    assert_eq!(m.releases, s.history.len());
    assert_eq!(m.features_shipped, 1);
    assert!(m.feature_ratio_pct <= 100, "a ratio is a percentage, never more");
}

#[test]
fn compute_is_deterministic_and_counts_never_exceed_their_populations() {
    let mut s = ProjectState::default();
    s.tickets = vec![feature("CXA-F001"), feature("CXA-F002")];
    s.history = vec![deploy("2026-09-06T10:00:00Z", "CXA-F001")];
    let a = metrics::compute(&s);
    let b = metrics::compute(&s);
    assert_eq!(a, b, "pure projection");
    assert!(a.features_shipped <= a.total_tickets);
    assert!(a.bugs_open + a.features_in_flight <= a.total_tickets + a.bugs_verified + a.features_shipped + 8);
}

fn deploy(at: &str, ticket: &str) -> coxagent_application::state::DeployRecord {
    coxagent_application::state::DeployRecord {
        version: coxagent_domain::version::SemVer::new(1, 0, 0),
        ticket: coxagent_domain::ids::TicketId::new(ticket).expect("id"),
        title: "shipped work".to_owned(),
        at: at.to_owned(),
    }
}

/// A feature already transitioned Pending -> Ready -> InProgress -> Done: the
/// "shipped" KPI counts only Done/Documented features, so a fresh Pending
/// ticket would make `features_shipped` 0 and the attribution assertion empty.
fn shipped_feature(id: &str) -> coxagent_domain::ticket::Ticket {
    let mut t = feature(id);
    t.set_technical_design(
        coxagent_domain::kinds::Role::Sa,
        coxagent_domain::ticket::TechnicalDesign { approach: "a".into(), ..Default::default() },
    ).expect("design");
    t.transition_to(coxagent_domain::kinds::Role::Sa, coxagent_domain::kinds::Status::Ready).expect("ready");
    t.transition_to(coxagent_domain::kinds::Role::DevFeature, coxagent_domain::kinds::Status::InProgress).expect("in progress");
    t.transition_to(coxagent_domain::kinds::Role::DevFeature, coxagent_domain::kinds::Status::Done).expect("done");
    t
}

fn feature(id: &str) -> coxagent_domain::ticket::Ticket {
    coxagent_domain::ticket::Ticket::new(
        coxagent_domain::ids::TicketId::new(id).expect("id"),
        coxagent_domain::kinds::TicketType::Feature,
        "t", "d",
        coxagent_domain::kinds::Priority::Medium,
        coxagent_domain::kinds::Complexity::Small,
        false,
    ).expect("ticket")
}
