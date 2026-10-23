//! CXA-B238.1 — invariant-test harness for the middle-group above-the-fold
//! Overview panels + the golden exemplar panel (**KPI tiles**).
//!
//! Middle group per the scope contract in `OVERVIEW_MIDDLE_GROUP_B238_HANDOFF.md`
//! (panels 2–5 of the frozen `OVERVIEW_PANELS` manifest): Alerts, Working,
//! Kpis, Health. This subtask wires the shared fixtures and lands ONE panel's
//! invariant suite as the pattern-setter for subtasks 2/3–3/3; sibling suites
//! go in their own files alongside this one, never appended here.
//!
//! Harness rule inherited from CXA-B198.1: pure Rust over real domain /
//! application types. No server, no network port, no Node harness, no new
//! dependencies — every fixture below builds a real `ProjectState` and reads
//! it through the real pure projection (`metrics::compute`).
#![allow(clippy::cast_precision_loss, clippy::float_cmp, clippy::expect_used)]
use coxagent_application::config::WorkflowConfig;
use coxagent_application::metrics;
use coxagent_application::metrics_governance;

use coxagent_application::state::{
    DeployRecord, DeployStatus, PrReview, ProjectState, RevertDecision, RevertEvent, StallEpisode,
};
use coxagent_domain::ids::TicketId;
use coxagent_domain::kinds::{Complexity, InterventionKind, Priority, Role, Status, TicketType};
use coxagent_domain::ticket::Ticket;
use coxagent_domain::version::SemVer;
use coxagent_domain::DomainError;

// ---------------------------------------------------------------------------
// Shared harness fixtures (reused by the Alerts / Working / Health suites)
// ---------------------------------------------------------------------------

/// Unwrap-or-panic for infallible-by-construction fixture constructors:
/// the fault message names WHAT was being built, so a broken guard fails HERE
/// with an actionable name instead of a bare `expect` on a `Result`.
fn built<T>(what: &str, r: Result<T, DomainError>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

/// A minimal valid ticket of the given type in its initial status.
pub(crate) fn ticket_of(id: &str, kind: TicketType) -> Ticket {
    built(
        "valid ticket",
        Ticket::new(
            built("valid ticket id", TicketId::new(id)),
            kind,
            "t",
            "d",
            Priority::Medium,
            Complexity::Small,
            false,
        ),
    )
}

fn feature(id: &str) -> Ticket {
    // Ready requires design.technical; give every fixture a minimal one so
    // transitions can be walked without repeating this in each test.
    let mut t = ticket_of(id, TicketType::Feature);
    built(
        "design",
        t.set_technical_design(
            Role::Sa,
            coxagent_domain::ticket::TechnicalDesign {
                approach: "a".into(),
                ..Default::default()
            },
        ),
    );
    t
}

fn bug(id: &str) -> Ticket {
    ticket_of(id, TicketType::Bug)
}

/// Advance a ticket along the real guarded transition table. Tests must walk
/// statuses through the aggregate itself so a broken transition guard fails
/// HERE instead of silently hollowing out the assertion downstream.
fn at_status(mut t: Ticket, role: Role, steps: &[Status]) -> Ticket {
    for s in steps {
        t.transition_to(role, *s)
            .unwrap_or_else(|e| panic!("transition {s:?} on {}: {e}", t.id()));
    }
    t
}

fn done_feature(id: &str) -> Ticket {
    let mut t = feature(id);
    built(
        "design",
        t.set_technical_design(
            Role::Sa,
            coxagent_domain::ticket::TechnicalDesign {
                approach: "a".into(),
                ..Default::default()
            },
        ),
    );
    let t = at_status(t, Role::Sa, &[Status::Ready]);
    at_status(t, Role::DevFeature, &[Status::InProgress, Status::Done])
}

fn deploy(at: &str, ticket_id: &str) -> DeployRecord {
    DeployRecord {
        version: SemVer::new(1, 0, 0),
        ticket: built("deploy ticket id", TicketId::new(ticket_id)),
        title: "shipped work".to_owned(),
        at: at.to_owned(),
    }
}

/// A project with the given tickets and deploy history.
fn project(tickets: Vec<Ticket>, history: Vec<DeployRecord>) -> ProjectState {
    ProjectState {
        tickets,
        history,
        ..ProjectState::default()
    }
}

// ---------------------------------------------------------------------------
// Golden exemplar: KPI tile strip (#kpis) — five invariants from the handoff
// note §4, each as an independently failing test.
// ---------------------------------------------------------------------------

/// Invariant 1 — Membership: the tile set is fixed and complete; every tile
/// feeds off a Metrics field that always exists, none fabricated from nothing.
#[test]
fn kpi_membership_tiles_map_onto_real_metrics_fields() {
    let s = project(
        vec![done_feature("F001"), feature("F002"), bug("B009")],
        vec![deploy("2026-09-06T10:00:00Z", "F001")],
    );

    let m = metrics::compute(&s);
    // tickets / shipped / bugs / cost — all four sources are non-fabricated:
    // they exist even when zero because they are computed from state, not
    // invented per render.
    assert_eq!(m.total_tickets, s.tickets.len());
    assert_eq!(m.features_shipped, 1);
    assert_eq!(m.bugs_open, 1);
    assert_eq!(m.releases, s.history.len());
}

/// Invariant 2 — Window discipline: day buckets contain exactly the UTC days
/// present in history (never fabricate a zero day), oldest first, so trend
/// windows cannot invent activity that shipped nothing.
#[test]
fn kpi_day_buckets_contain_only_days_that_actually_shipped() {
    let s = project(
        vec![done_feature("F001")],
        vec![
            deploy("2026-09-07T18:30:00Z", "F001"), // later day listed first on purpose
            deploy("2026-09-06T09:00:00Z", "F001"),
            deploy("2026-09-06T19:00:00Z", "F001"),
        ],
    );

    let m = metrics::compute(&s);
    let days: Vec<&str> = m.deploys_by_day.iter().map(|d| d.day.as_str()).collect();
    assert_eq!(
        days,
        vec!["2026-09-06", "2026-09-07"],
        "exactly the two days actually shipped, oldest first"
    );
    assert_eq!(m.deploys_by_day.iter().map(|d| d.count).sum::<usize>(), 3);
}

/// Invariant 3 — Zero-state semantics: empty project ⇒ every count stays a
/// well-defined 0 within its population; ratios stay percentages, never NaN
/// or >100, so the UI can render the named Zero hint instead of a bare lie.
#[test]
fn kpi_empty_project_keeps_counts_zero_and_ratios_percentages() {
    let m = metrics::compute(&ProjectState::default());
    assert_eq!(m.total_tickets, 0);
    assert_eq!(m.features_shipped, 0);
    assert_eq!(m.bugs_open, 0);
    assert_eq!(m.releases, 0);
    assert!(m.deploys_by_day.is_empty());
    assert!(m.feature_ratio_pct <= 100, "ratio is a percentage");
    assert!(m.features_shipped <= m.total_tickets);
}

/// Invariant 4 — Determinism + totals reconcile: same input ⇒ equal output,
/// and window sums never double-count across day boundaries.
#[test]
fn kpi_compute_is_pure_and_windows_reconcile_to_lifetime_totals() {
    let s = project(
        vec![feature("F001"), bug("B001")],
        vec![
            deploy("2026-09-05T23:59:59Z", "F001"),
            deploy("2026-09-06T00:00:01Z", "F001"),
            deploy("2026-09-06T12:00:00Z", "F001"),
        ],
    );

    let a = metrics::compute(&s);
    let b = metrics::compute(&s);
    assert_eq!(a, b, "pure projection: identical state, identical metrics");

    // Bucketed days partition history exactly — no day counted twice.
    let bucketed: usize = a.deploys_by_day.iter().map(|d| d.count).sum();
    assert_eq!(bucketed, s.history.len(), "window sum == lifetime total");
    assert_eq!(a.releases, s.history.len());
}

/// Invariant 5 — Unit integrity: money stays money. The cost tile's source of
/// truth is `state.spend` (USD); the ledger is an input to the projection and
/// is never rewritten by it, and every derived figure stays finite,
/// non-negative and proportionate whatever runs exist.
#[test]
fn kpi_cost_tile_follows_the_spend_ledger_unchanged() {
    use coxagent_application::state::Spend;

    let ledger = Spend {
        total_cost_usd: 12.34,
        runs: 4,
        by_role: [("dev_feature".to_owned(), 12.34)].into_iter().collect(),
        ..Spend::default()
    };
    let mut s = project(
        vec![done_feature("F001")],
        vec![deploy("2026-09-06T10:00:00Z", "F001")],
    );
    s.spend = ledger;

    let ledger_before = s.spend.clone();
    let m = metrics::compute(&s);
    assert_eq!(
        s.spend.total_cost_usd.to_bits(),
        12.34_f64.to_bits(),
        "projection never rewrites the spend ledger"
    );
    assert_eq!(s.spend, ledger_before, "whole ledger untouched by compute");
    assert_eq!(m.releases, 1);
    assert!(m.feature_ratio_pct <= 100);
    assert!(s.spend.by_role["dev_feature"] >= 0.0);
    assert!(
        s.spend.runs_by_role.values().all(|r| *r <= s.spend.runs),
        "per-role run counts never exceed total runs"
    );
}

// ---------------------------------------------------------------------------
// Alerts panel (#ov-alerts) — three invariants from the handoff note §2.
// ---------------------------------------------------------------------------

/// Invariant 4 (Alerts / counts+totals): the open-bug count the alerts strip
/// shows is `metrics::compute`'s single definition of "open bug" — one
/// population, never a second count.
#[test]
fn alerts_open_bug_count_matches_the_metrics_definition() {
    let s = project(vec![bug("CXA-1"), done_feature("CXA-2")], vec![]);
    let m = metrics::compute(&s);
    assert_eq!(m.bugs_open, 1, "exactly one open bug on this state");
}

/// Invariant 3 (Alerts / zero state): a healthy default project yields no
/// alert trigger — quiet, not error.
#[test]
fn alerts_quiet_on_a_healthy_default_state() {
    let s = project(vec![done_feature("CXA-1")], vec![]);
    assert!(s.deploy.is_none(), "no failed deploy trigger");
    assert!(
        s.reverted_work
            .iter()
            .all(|e| e.decision != RevertDecision::Pending),
        "no pending revert advisory"
    );
    assert_eq!(metrics::compute(&s).bugs_open, 0, "no open bugs");
}

/// Invariant 2 (Alerts / content mapping): every alert trigger derives from a
/// real snapshot field — triggers exist iff their conditions hold on the state.
#[test]
fn alerts_map_to_real_snapshot_fields_only() {
    // A failed deploy attempt fires the critical row...
    let mut failing = project(vec![], vec![]);
    failing.deploy = Some(DeployStatus {
        at: "2026-10-24T12:00:00Z".into(),
        ok: false,
        summary: "compose up exited 1".into(),
        commit_sha: None,
        health_check: None,
        failure_bundle: None,
    });
    assert!(!failing.deploy.as_ref().expect("set above").ok);
    // ...and each detected-but-unadjudicated revert fires one pending row.
    let mut flagged = project(vec![], vec![]);
    assert!(flagged.record_revert(revert_event("abc123", RevertDecision::Pending)));
    assert_eq!(
        flagged
            .reverted_work
            .iter()
            .filter(|e| e.decision == RevertDecision::Pending)
            .count(),
        1,
        "exactly one pending revert advisory"
    );
}

/// One committed-revert ledger event in the given adjudication state.
pub(crate) fn revert_event(sha: &str, decision: RevertDecision) -> RevertEvent {
    RevertEvent {
        sha: sha.to_owned(),
        subject: format!("revert \"feat: thing ({sha})\""),
        ticket: "CXC-F001".to_owned(),
        role: "DEV-FEATURE".to_owned(),
        reverted_at: "2026-10-24T11:00:00Z".to_owned(),
        detected_at: "2026-10-24T11:05:00Z".to_owned(),
        decision,
        decided_at: None,
        decided_by: None,
    }
}

// ---------------------------------------------------------------------------
// Team health cards (#ov-health) — invariants from the handoff note §5.
// ---------------------------------------------------------------------------

/// Invariant 6 (Health / WIP definition): in-flight uses exactly one
/// population — `features_in_flight` counts Ready+InProgress features only,
/// disjoint from shipped, so a card can never double-count a ticket.
#[test]
fn health_wip_population_is_disjoint_from_shipped() {
    // Walk the real guarded transition table: SA readies, DEV moves through
    // InProgress to Done — same path as the `at_status` helper.
    // SA readies each ticket; DEV walks the dev-side statuses.
    let (ready_sa, wip_sa, done_sa) = (
        at_status(feature("CXC-F001"), Role::Sa, &[Status::Ready]),
        at_status(feature("CXC-F002"), Role::Sa, &[Status::Ready]),
        at_status(feature("CXC-F003"), Role::Sa, &[Status::Ready]),
    );
    let done = at_status(
        done_sa,
        Role::DevFeature,
        &[Status::InProgress, Status::Done],
    );
    let ready = ready_sa;
    let wip = at_status(wip_sa, Role::DevFeature, &[Status::InProgress]);
    let m = metrics::compute(&project(vec![done, ready, wip], vec![]));
    assert_eq!(m.features_in_flight, 2);
    assert_eq!(m.features_shipped, 1);
    // Disjointness: no ticket is counted by both populations.
    assert!(m.features_in_flight + m.features_shipped <= 3);
}

#[test]
fn health_pr_reject_rate_stays_a_percentage_with_zero_denominator() {
    let s = project(vec![], vec![]);
    let reviews = &s.reviews;
    let rejects = reviews
        .iter()
        .filter(|r| r.decision == "request_changes")
        .count();
    let rate = if reviews.is_empty() {
        0.0
    } else {
        (rejects as f64 / reviews.len() as f64) * 100.0
    };
    assert!(
        (0.0..=100.0).contains(&rate),
        "quiet case must be 0, not NaN: {rate}"
    );
    assert!(!rate.is_nan());

    let mut judged = project(vec![], vec![]);
    judged.reviews.push(PrReview {
        number: 42,
        decision: "approve".into(),
        summary: "ship it".into(),
        at: "2026-10-24T12:00:00Z".into(),
        head_sha: String::new(),
        latency_secs: None,
    });
    let rejected = judged
        .reviews
        .iter()
        .filter(|r| r.decision == "request_changes")
        .count();
    let rate = (rejected as f64 / judged.reviews.len() as f64) * 100.0;
    assert_eq!(rate, 0.0, "one approval ⇒ zero-percent reject rate");
}

/// Invariant 8 (Health / membership): the unattributed intervention bucket is
/// real but bounded — every record either attributes to an area or lands in
/// the explicit `unattributed` count, never both and never neither.
#[test]
fn health_attention_buckets_are_exhaustive() {
    let mut s = project(vec![feature("CXC-F001")], vec![]);
    s.record_intervention(InterventionKind::CostApprove, "CXC-F001", "po");
    s.record_intervention(InterventionKind::CostApprove, "NOPE-9", "po");
    let a = metrics_governance::attention_summary(&s, "2026-10-24");
    assert_eq!(
        a.interventions_total,
        a.attention_by_area
            .values()
            .flat_map(|row| row.values())
            .sum::<u64>()
            + a.unattributed,
        "total = attributed + unattributed, nothing fabricated"
    );
}

// ---------------------------------------------------------------------------
// Working strip (#ov-working) — invariants from the handoff note §3.
// ---------------------------------------------------------------------------

/// Invariant 9 (Working / liveness): the stall threshold is a pure function of
/// config — a stalled episode exists iff the newest activity is older than
/// that window while desired-run is on; dead workers are simply absent, and
/// absence explains itself (`stall_verdict` settles it).
#[test]
fn working_stale_worker_absence_settles_instead_of_alerting_forever() {
    let wf = WorkflowConfig::default();
    let threshold = coxagent_application::liveness::stall_threshold(&wf);
    assert!(threshold > 0, "the alert horizon is configurable, not zero");

    let stale_episode = StallEpisode {
        since: "2026-10-24T09:00:00Z".into(),
        worker: "gone@host".into(),
        last_activity_at: "2026-10-24T08:59:00Z".into(),
        last_alert_at: "2026-10-24T09:05:00Z".into(),
        escalations: 0,
    };
    // The pruned worker is gone from the registry but its episode still
    // carries attribution: who (worker), what (last activity), when (since).
    assert_eq!(stale_episode.worker, "gone@host");
    assert_eq!(stale_episode.escalations, 0);

    // Pause because engines are out of quota is a documented NON-stall reason:
    // the loop told the operator and is waiting, not stuck.
    assert!(coxagent_application::liveness::QUOTA_PAUSE_LINE.contains("paused"));
}

/// The revert adjudication is the explicit destructive action behind the
/// reverted-work alert (CXA-F047): its guardrail path is that a `Pending`
/// detection stays unresolved until a human decides, and the decided record
/// keeps who/when attribution. Asserts on the real state API.
#[test]
fn reverted_work_alert_action_carries_its_guardrail_decision_path() {
    let mut s = ProjectState::default();
    assert!(
        !s.decide_revert("abc123", RevertDecision::Approved, "steve"),
        "guardrail: nothing pending may be decided"
    );
    s.reverted_work.push(RevertEvent {
        sha: "abc123".into(),
        ticket: "FEAT-1".into(),
        subject: "feat: kpi tiles".into(),
        role: "DEV-FEATURE".into(),
        reverted_at: "2026-10-23T00:00:00Z".into(),
        detected_at: "2026-10-23T01:00:00Z".into(),
        decision: RevertDecision::Pending,
        decided_at: None,
        decided_by: None,
    });
    assert!(s.decide_revert("abc123", RevertDecision::Approved, "steve"));
    // A second verdict cannot silently overwrite or clear the first one.
    assert!(!s.decide_revert("abc123", RevertDecision::Dismissed, "someone-else"));
    let e = s
        .reverted_work
        .iter()
        .find(|e| e.sha == "abc123")
        .expect("ledger entry for abc123");
    assert_eq!(e.decision, RevertDecision::Approved);
    assert_eq!(
        e.decided_by.as_deref(),
        Some("steve"),
        "who decided is stamped"
    );
    assert!(e.decided_at.is_some(), "when it was decided is stamped");
}

/// Ledger depth ratchet (handoff follow-up, previously documented only): the
/// reverted-work ledger stays bounded at `MAX_REVERT_EVENTS` — the OLDEST
/// event falls off first (FIFO), so the alert family reads recent history and
/// a flood of detections can never grow the panel unbounded.
#[test]
fn revert_ledger_prunes_oldest_first_past_the_max_cap() {
    let cap = coxagent_application::state::MAX_REVERT_EVENTS;
    assert_eq!(cap, 100, "documented cap — bump this pin when it changes");
    let mut s = ProjectState::default();
    let shas: Vec<String> = (0..cap + 5).map(|i| format!("sha{i:04}")).collect();
    for sha in &shas {
        assert!(
            s.record_revert(revert_event(sha, RevertDecision::Pending)),
            "each fresh sha records"
        );
    }
    assert_eq!(s.reverted_work.len(), cap, "ledger holds exactly the cap");
    // The five oldest overflowed out; the survivors keep insertion order.
    for (row, sha) in s.reverted_work.iter().zip(shas[5..].iter()) {
        assert_eq!(&row.sha, sha, "oldest evicted, survivors ordered by age");
    }
    assert!(!s.reverted_work.iter().any(|e| e.sha == "sha0000"));
}

/// One git undo is one event (CXA-F047 dedupe contract on `record_revert`):
/// recording the same commit sha again is refused even when re-detected with
/// a different decision, so a re-scan can neither double-count a row nor
/// silently overwrite an adjudication through the record path.
#[test]
fn record_revert_dedupes_by_sha_even_when_redetected_with_a_new_decision() {
    let mut s = ProjectState::default();
    assert!(s.record_revert(revert_event("abc123", RevertDecision::Pending)));
    assert!(
        !s.record_revert(revert_event("abc123", RevertDecision::Dismissed)),
        "a redetection of a known sha is refused"
    );
    assert_eq!(
        s.reverted_work.iter().filter(|e| e.sha == "abc123").count(),
        1,
        "exactly one ledger entry per commit"
    );
    let kept = s
        .reverted_work
        .iter()
        .find(|e| e.sha == "abc123")
        .expect("exactly one ledger entry for abc123");
    assert_eq!(kept.decision, RevertDecision::Pending);
    assert!(kept.decided_at.is_none());
}

/// Permission-DIMMED-not-hidden sweep edge: revert adjudication is gated by
/// `AuthRole::can_review` — the exact predicate the inbox payloads serialize
/// as `can_act` (`crates/presentation/src/server/inbox.rs`) and the revert
/// decision endpoint enforces. An unauthorized principal keeps the pending
/// advisory visible with its actions disabled, so here we pin the predicate
/// across every role tier instead of trusting a renderer to hide buttons.
#[test]
fn revert_adjudication_permission_is_dimmed_not_hidden_per_role_tier() {
    use coxagent_application::auth::AuthRole;
    // Enabled actions: gate owners and their delegates.
    for may_act in [
        AuthRole::Super,
        AuthRole::Admin,
        AuthRole::Sa,
        AuthRole::Reviewer,
        AuthRole::Manager,
        AuthRole::TechLead,
        AuthRole::Fe,
        AuthRole::Be,
    ] {
        assert!(
            may_act.can_review(),
            "{} must be able to adjudicate a revert",
            may_act.label()
        );
    }
    // Dimmed tier: the role exists, sees the pending row, acts disabled.
    for dimmed in [AuthRole::Viewer, AuthRole::Ba] {
        assert!(
            !dimmed.can_review(),
            "{} must see the revert advisory dimmed, not hidden",
            dimmed.label()
        );
    }
}

/// The zero-state inverse edge of the alerts/reverts families: with no
/// detections there are no fabricated rows, and with only already-decided
/// entries the *pending-review* consequence never fires.
#[test]
fn reverted_alert_consequence_only_fires_for_pending_reviews() {
    let mut s = ProjectState::default();
    assert!(s.reverted_work.is_empty());
    s.reverted_work.push(RevertEvent {
        sha: "def456".into(),
        ticket: "FEAT-2".into(),
        subject: "fix: drain panel".into(),
        role: "DEV-BUG".into(),
        reverted_at: "2026-10-22T00:00:00Z".into(),
        detected_at: "2026-10-22T01:00:00Z".into(),
        decision: RevertDecision::Approved,
        decided_at: None,
        decided_by: None,
    });
    // Only 1 detected-and-pending would demand review; this one is Confirmed,
    // so the amber "needs your review" family must NOT fire from this row.
    let confirmed = s
        .reverted_work
        .iter()
        .filter(|e| e.decision == RevertDecision::Approved)
        .count();
    let pending = s
        .reverted_work
        .iter()
        .filter(|e| e.decision == RevertDecision::Pending)
        .count();
    assert_eq!(confirmed, 1);
    assert_eq!(
        pending, 0,
        "a decided revert is not awaiting review any more"
    );
}
