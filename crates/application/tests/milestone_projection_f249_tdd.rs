//! TDD tests for CXA-F249 — Forward milestone-completion projection with
//! blocked-scope surfacing.
//!
//! Acceptance criteria pinned as executable tests over the public API of the
//! pure read model (`coxagent_application::milestone_projection`) and the
//! selection predicates it reuses (`coxagent_application::selection`). Every
//! fixture is built through the legal domain aggregate API and real persisted
//! shapes only — no server, no host harness, no network port, no fabricated
//! persisted fields.
//!
//! The SA design on file (.coxagent/CXA-F249-design.json) rules the shape, and
//! it settles the gaps a prior draft of this file had escalated:
//! - NO ticket↔milestone attribution exists anywhere (`Milestone` carries only
//!   name/goal/target_version/goal_complete/fulfilled; `Ticket` has no
//!   milestone field), so per-milestone completion DATES and remaining-counts
//!   would be fabricated data — the projection is strictly a classification
//!   read model. "Insufficient data" is therefore structural, not a branch:
//!   the wire has no date field at all (pinned below), so it can never
//!   fabricate a date regardless of velocity history.
//! - AC2's staleness rule ("no status change for >= configured days") has no
//!   data source (`Ticket` persists no status-change timestamp, no dependency-
//!   staleness knob exists in config); the SA ruled blockers surface through
//!   the existing selection predicates (`deps_satisfied` / `in_dev_scope`)
//!   instead — pinned here via the public `selection::work_blockers`.
//!
//! AC → test map:
//! - AC1 (per-milestone projection over the open work): the classification
//!   branches in the `milestone_projection` module's unit tests and, through
//!   the public API, `ac1_the_projection_reads_purely_and_per_milestone`.
//! - AC2 (blocking risks named and attributed to the affected work):
//!   [`ac2_blockers_carry_named_tickets_and_machine_readable_reasons`].
//! - AC3 (forecast updates whenever /state changes):
//!   [`ac3_the_forecast_recomputes_when_state_changes`].
//! - AC4 (edge cases: empty scope; insufficient history; fully shipped):
//!   [`ac4_empty_scope_projects_nothing`] and
//!   [`ac4_fully_shipped_milestones_read_released_not_missing`].

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::milestone_projection::projection_report;
use coxagent_application::selection::{BlockedReason, work_blockers};
use coxagent_application::state::{Milestone, ProjectState, Sprint};
use coxagent_domain::{
    Complexity, Priority, Role, SemVer, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};

// ---------------------------------------------------------------------------
// Fixtures — built ONLY through the domain aggregate's legal API and the
// persisted shapes that exist today.
// ---------------------------------------------------------------------------

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

/// A feature walked to `status` along the only legal edges. Features start
/// `Pending` (`Ticket::new`); `Ready` needs the technical design;
/// `InProgress` is DEV-FEATURE's edge.
fn feature_at(id: &str, status: Status) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Feature,
        format!("feature {id}"),
        "fixture",
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("ticket");
    if status != Status::Pending {
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("attach design");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        if status == Status::InProgress {
            t.transition_to(Role::DevFeature, Status::InProgress)
                .expect("claim");
        }
    }
    t
}

fn milestone(name: &str, target: &str, goal_complete: bool, fulfilled: bool) -> Milestone {
    Milestone {
        name: name.to_owned(),
        goal: format!("the {name} goal"),
        target_version: target.to_owned(),
        goal_complete,
        fulfilled,
    }
}

fn state_with(current: &str, milestones: Vec<Milestone>) -> ProjectState {
    ProjectState {
        current_version: SemVer::parse(current).expect("version"),
        milestones,
        ..ProjectState::default()
    }
}

fn scrum(committed: &[&str]) -> Sprint {
    Sprint {
        number: 1,
        goal: String::new(),
        started_cycle: 0,
        length_cycles: 10,
        committed: committed.iter().copied().filter_map(|c| TicketId::new(c).ok()).collect(),
        started_at: String::new(),
        bug_burn_floor: None,
    }
}

// ---------------------------------------------------------------------------
// AC1 — the projection is a pure, per-milestone read over ProjectState.
// ---------------------------------------------------------------------------

#[test]
fn ac1_the_projection_reads_purely_and_per_milestone() {
    // The AC's given: a milestone whose goal is NOT complete and non-verified
    // work in flight (pending + in-progress tickets).
    let mut state = state_with("0.5.2", vec![milestone("Beta", "0.9.0", false, false)]);
    state.tickets = vec![
        feature_at("FEAT-A", Status::Pending),
        feature_at("FEAT-B", Status::InProgress),
    ];
    // An open sprint that committed neither ticket: the in-flight work is
    // pending/started but not dev-scope-aligned, exactly the forward risk the
    // AC pairs with the milestone row.
    state.sprint = Some(scrum(&[]));
    let before = state.clone();

    let report = projection_report(&state);

    // A pure read: no mutation, deterministic over the same snapshot — a
    // cached or wall-clock-driven result would make the dashboard contradict
    // itself between renders.
    assert_eq!(
        before, state,
        "AC1: the projection never mutates the state it reads"
    );
    assert_eq!(
        projection_report(&state),
        report,
        "AC1: deterministic over the same state — no IO, no drift"
    );

    // One milestone in, one per-milestone row out, carrying its identity and
    // its classification against the current version.
    assert_eq!(report.milestones.len(), 1, "AC1: one row per milestone");
    let row = &report.milestones[0];
    assert_eq!(row.name, "Beta");
    assert_eq!(row.target_version, "0.9.0");
    assert!(!row.released && !row.reached, "AC1: the active row: {row:?}");

    // The in-flight work IS visible — as named blocked-scope risks (AC2's
    // surface), never as a per-milestone membership the model does not have.
    assert!(
        !report.blocked_tickets.is_empty(),
        "AC1: the open work the milestone's sprints must clear is surfaced"
    );
}

// ---------------------------------------------------------------------------
// AC2 — blocking risks are named tickets with machine-readable reasons.
// ---------------------------------------------------------------------------

#[test]
fn ac2_blockers_carry_named_tickets_and_machine_readable_reasons() {
    let mut blocked = feature_at("FEAT-B", Status::Ready);
    blocked.add_dependency(Role::Sa, tid("BUG-1")).expect("dep");
    let bug = Ticket::new(
        tid("BUG-1"),
        TicketType::Bug,
        "the open blocker",
        "",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("ticket");

    // Kanban (no sprint): in scope, so the unsatisfied dependency is the only
    // reason the ticket cannot move.
    let kanban = ProjectState {
        tickets: vec![bug.clone(), blocked.clone()],
        ..ProjectState::default()
    };
    let kanban_rows = work_blockers(&kanban);
    let rows: Vec<_> = kanban_rows
        .iter()
        .map(|b| (b.id.as_str(), b.title.as_str(), b.reason))
        .collect();
    assert_eq!(
        rows,
        vec![("FEAT-B", "feature FEAT-B", BlockedReason::DependencyUnsatisfied)],
        "AC2: the blocker is the named ticket, the reason machine-readable: {rows:?}"
    );

    // Scrum without a commit: the same feature is ALSO out of dev scope —
    // both honest reasons, one entry each.
    let scrum_uncommitted = ProjectState {
        tickets: vec![bug, blocked],
        sprint: Some(scrum(&[])),
        ..ProjectState::default()
    };
    let scrum_rows = work_blockers(&scrum_uncommitted);
    let reasons: Vec<_> = scrum_rows
        .iter()
        .map(|b| (b.id.as_str(), b.reason))
        .collect();
    assert_eq!(
        reasons,
        vec![
            ("FEAT-B", BlockedReason::DependencyUnsatisfied),
            ("FEAT-B", BlockedReason::OutOfDevScope),
        ],
        "AC2: a ticket blocked two ways says so twice, not once: {reasons:?}"
    );

    // And on the wire the reason is the contracted kebab-case string.
    let v = serde_json::to_value(projection_report(&scrum_uncommitted)).expect("serialize");
    assert_eq!(v["blocked_tickets"][0]["reason"], "dependency-unsatisfied");
    assert_eq!(v["blocked_tickets"][1]["reason"], "out-of-dev-scope");
}

// ---------------------------------------------------------------------------
// AC3 — the forecast updates whenever /state changes.
// ---------------------------------------------------------------------------

#[test]
fn ac3_the_forecast_recomputes_when_state_changes() {
    // The dashboard read model must be derived fresh from the state it is
    // handed — never cached across changes. The flips the AC names are visible
    // without a formula: a version crossing the target moves the milestone
    // from "working toward" to "reached", and a landed dependency clears the
    // blocker it named.
    let before = state_with("0.5.1", vec![milestone("Beta", "0.5.2", true, false)]);
    assert!(
        !projection_report(&before).milestones[0].reached,
        "AC3: before the change the milestone is still being worked toward"
    );

    let after = state_with("0.5.2", vec![milestone("Beta", "0.5.2", true, false)]);
    assert!(
        projection_report(&after).milestones[0].reached,
        "AC3: after /state changes the same read renders the milestone reached"
    );

    // The blocker half: a dependency that does not exist is a named risk,
    // and once that dependency lands (bug reaches Verified) the risk clears
    // on the next read — no cache in the way.
    let mut dep_missing = ProjectState {
        tickets: vec![feature_at("FEAT-B", Status::Ready)],
        ..ProjectState::default()
    };
    dep_missing.tickets[0]
        .add_dependency(Role::Sa, tid("BUG-1"))
        .expect("dep");
    let mut resolved = dep_missing.clone();
    let mut bug = Ticket::new(
        tid("BUG-1"),
        TicketType::Bug,
        "the open blocker",
        "",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    bug.transition_to(Role::DevBug, Status::InProgress)
        .expect("claim");
    bug.transition_to(Role::DevBug, Status::Fixed).expect("fixed");
    bug.transition_to(Role::Test, Status::Verified)
        .expect("verified");
    resolved.tickets.push(bug);
    assert!(
        work_blockers(&dep_missing)
            .iter()
            .any(|b| b.reason == BlockedReason::DependencyUnsatisfied),
        "AC3: the missing dependency is a named risk first"
    );
    assert!(
        !work_blockers(&resolved)
            .iter()
            .any(|b| b.reason == BlockedReason::DependencyUnsatisfied),
        "AC3: once the dependency lands the risk clears on the next read"
    );
}

// ---------------------------------------------------------------------------
// AC4 — the conservative edges.
// ---------------------------------------------------------------------------

#[test]
fn ac4_empty_scope_projects_nothing() {
    // An empty roadmap is not an error and never fabricates rows.
    let empty = ProjectState::default();
    let report = projection_report(&empty);
    assert!(
        report.milestones.is_empty(),
        "AC4: empty milestone scope -> no projection"
    );

    // Zero or single-sprint velocity history: the SA design ruled the
    // projection carries NO date at all — "insufficient data" is structural,
    // so the conservative answer cannot regress into a fabricated date. The
    // wire is pinned to exactly the contract keys; a date field appearing
    // here would be the fabrication the AC warns against.
    let v = serde_json::to_value(&report).expect("serialize");
    let keys: std::collections::BTreeSet<_> =
        v.as_object().expect("object").keys().cloned().collect();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from([
            "blocked_tickets".to_owned(),
            "current_version".to_owned(),
            "milestones".to_owned(),
        ]),
        "AC4: no date field exists on the wire, so no fabricated date can: {keys:?}"
    );
}

#[test]
fn ac4_fully_shipped_milestones_read_released_not_missing() {
    // A fully shipped milestone stays in the read model with its terminal
    // classification — the dashboard renders the whole roadmap, and the row
    // can never contradict the release pipeline that fulfilled it.
    let mixed = state_with(
        "0.5.2",
        vec![
            milestone("Shipped", "0.5.0", true, true),
            milestone("Open", "0.9.0", false, false),
        ],
    );
    let report = projection_report(&mixed);
    let rows: Vec<_> = report
        .milestones
        .iter()
        .map(|r| (r.name.as_str(), r.released, r.reached))
        .collect();
    assert_eq!(
        rows,
        vec![("Shipped", true, true), ("Open", false, false)],
        "AC4: shipped reads released+reached, open reads neither: {rows:?}"
    );
}
