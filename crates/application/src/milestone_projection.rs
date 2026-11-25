//! Forward milestone-completion projection (CXA-F249; scope attribution
//! CXA-F252) — a pure read model over `ProjectState`: where each declared
//! milestone stands against the current version, which open work cannot move
//! right now, and which committed scope is attributed to the milestone the
//! sprints are working toward.
//!
//! No IO, no mutation, no fabrication. The persisted model still carries no
//! ticket↔milestone link and no per-milestone date, so a date or a
//! remaining-count-against-velocity would be invented data; what IS derivable
//! — per the SA's CXA-F252 ruling — is committed scope: a ticket belongs to
//! the first unreleased milestone (the active target) iff it sits in the
//! active sprint's `committed` set. That is one pure function over state,
//! zero schema change, and it cannot contradict the release pipeline: the
//! active target must have a parseable `target_version`, the same parse
//! `RunReleasesUseCase` needs before it can ever fulfill the row, so an
//! absent or parse-broken roadmap yields no attribution at all. Milestone
//! classification still runs through EXACTLY the pipeline's gates
//! (fulfilled -> released; current_version >= parsed target), and blocked
//! work is surfaced through the same selection predicates the scheduler uses
//! (`selection::work_blockers`).

use serde::Serialize;

use crate::selection::{self, BlockedWork};
use crate::state::ProjectState;
use coxagent_domain::{SemVer, Status, TicketId};

/// One milestone's forward read model, classified purely from state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilestoneProjection {
    /// The milestone's identity on the wire: its `name`, which is already the
    /// de-facto id — the release pipeline matches rows by it and release tags
    /// carry it (CXA-F252). No new persisted field, no migration.
    pub id: String,
    pub name: String,
    pub target_version: String,
    pub goal_complete: bool,
    /// The release pipeline already tagged this milestone (`fulfilled`).
    pub released: bool,
    /// The current version has reached the target — or the milestone is
    /// released, which is definitionally reached.
    pub reached: bool,
    /// Committed scope attributed to this milestone by the derived rule
    /// (CXA-F252): the active sprint's committed tickets that still exist, in
    /// commit order. Empty on every row except the active target — there is
    /// only one, and Kanban (no sprint) attributes nothing.
    pub committed: Vec<TicketId>,
    /// The committed subset not yet delivered — the work the milestone still
    /// waits on. Delivered (`Done`/`Documented`/`Verified`) and `Rejected`
    /// work is closed; `Fixed` is deliberately open (built but awaiting the
    /// human verify gate), and so is `OnHold` (paused is not delivered).
    pub open: Vec<TicketId>,
    /// Completion percent for the roadmap's progress bars (CXA-F361), derived
    /// purely from the tickets attributed to this row: a released row is
    /// complete by the release pipeline's own record (100, even with zero
    /// attributed scope); the active target's bar is its committed scope's
    /// closed share — the same `closes_scope` set the `open` list is the
    /// complement of — rounded to an integer percent; a row with no
    /// attributed scope reads 0, which is never NaN and never masquerades as
    /// either done or unstarted-by-choice.
    pub progress: u8,
}

/// The whole `GET /api/projects/:pid/milestones/projection` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilestoneProjectionReport {
    pub current_version: String,
    pub milestones: Vec<MilestoneProjection>,
    pub blocked_tickets: Vec<BlockedWork>,
}

/// The active target's position in the declared roadmap: the first unreleased
/// row, PROVIDED its release target parses — the same parse
/// `RunReleasesUseCase` needs before it can ever fulfill the row. A broken
/// target means the pipeline can never complete that milestone, so it is no
/// active target at all and never receives attributed scope (CXA-F252 AC3).
fn active_target_index(state: &ProjectState) -> Option<usize> {
    state
        .milestones
        .iter()
        .position(|m| !m.fulfilled)
        .filter(|&i| SemVer::parse(&state.milestones[i].target_version).is_ok())
}

/// The milestone a ticket's committed work is attributed to, DERIVED from
/// state — never persisted (CXA-F252): a ticket belongs to the first
/// unreleased milestone (the active target) iff it is in the active sprint's
/// `committed` set. Yields the milestone's name — the wire id. Kanban (no
/// sprint), uncommitted tickets, and absent or parse-broken roadmaps
/// attribute to nothing.
#[must_use]
pub fn attributed_milestone(state: &ProjectState, id: &TicketId) -> Option<String> {
    let sprint = state.sprint.as_ref()?;
    if !sprint.committed.contains(id) {
        return None;
    }
    active_target_index(state).map(|i| state.milestones[i].name.clone())
}

/// The active sprint's committed scope that still exists as tickets, in the
/// PO's commit order. Deleted tickets are gone from the board — they cannot
/// block shipping and must not haunt the milestone row.
fn committed_scope(state: &ProjectState) -> Vec<TicketId> {
    state
        .sprint
        .as_ref()
        .map(|s| {
            s.committed
                .iter()
                .filter(|id| state.ticket(id).is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// A committed ticket that no longer stands between the team and its
/// milestone: delivered (`Done`/`Documented`/`Verified` — the same settled
/// set the dependency gate accepts) or `Rejected` (dead, never shipping).
/// `Fixed` stays open on purpose: the work is built but parked at the human
/// verify gate — exactly the wait CXA-F252 exists to make visible — and
/// `OnHold` stays open because paused work still blocks the scope.
fn closes_scope(status: Status) -> bool {
    matches!(
        status,
        Status::Done | Status::Documented | Status::Verified | Status::Rejected
    )
}

/// Classify every declared milestone, keeping the PO-authored declared order
/// (ascending targets, enforced by milestone planning): the first unreleased
/// row is the active target the sprints are working toward, the unreleased
/// rows after it are ahead. A malformed target never reads as released or
/// reached — a broken roadmap must not masquerade as done work; an already
/// `fulfilled` milestone reads released regardless of target parseability
/// (the release gate checks `fulfilled` first, before it ever parses).
/// Committed scope attaches to the active target only, by POSITION — names
/// are the wire id, but the rule is "first unreleased row", not "first row
/// with that name".
#[must_use]
pub fn project_milestones(state: &ProjectState) -> Vec<MilestoneProjection> {
    let scope = committed_scope(state);
    let open_scope: Vec<TicketId> = scope
        .iter()
        .filter(|id| state.ticket(id).is_some_and(|t| !closes_scope(t.status())))
        .cloned()
        .collect();
    let active = active_target_index(state);
    state
        .milestones
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let released = m.fulfilled;
            let target = SemVer::parse(&m.target_version).ok();
            let reached = released || target.is_some_and(|t| state.current_version >= t);
            let on_active_target = active.is_some_and(|a| a == i);
            let row_scope = if on_active_target {
                scope.clone()
            } else {
                Vec::new()
            };
            let progress = if released {
                100
            } else if !row_scope.is_empty() {
                let closed = row_scope
                    .iter()
                    .filter(|id| state.ticket(id).is_some_and(|t| closes_scope(t.status())))
                    .count();
                // Round-half-up integer percent, no float drift. `closed` can
                // never exceed the divisor (both count the same list), so the
                // quotient is ≤ 100 — the conversion keeps that invariant
                // total, clamping an impossible overflow to the honest
                // ceiling rather than truncating.
                u8::try_from((closed * 100 + row_scope.len() / 2) / row_scope.len()).unwrap_or(100)
            } else {
                0
            };
            MilestoneProjection {
                id: m.name.clone(),
                name: m.name.clone(),
                target_version: m.target_version.clone(),
                goal_complete: m.goal_complete,
                released,
                reached,
                committed: row_scope.clone(),
                open: if on_active_target {
                    open_scope.clone()
                } else {
                    Vec::new()
                },
                progress,
            }
        })
        .collect()
}

/// Assemble the full read model: milestone classification plus blocked-scope
/// surfacing, one pure call so the handler stays load -> call -> Json.
#[must_use]
pub fn projection_report(state: &ProjectState) -> MilestoneProjectionReport {
    MilestoneProjectionReport {
        current_version: state.current_version.to_string(),
        milestones: project_milestones(state),
        blocked_tickets: selection::work_blockers(state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Sprint;
    use coxagent_domain::{
        Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
    };

    fn milestone(
        name: &str,
        target: &str,
        goal_complete: bool,
        fulfilled: bool,
    ) -> crate::state::Milestone {
        crate::state::Milestone {
            name: name.to_owned(),
            goal: format!("the {name} goal"),
            target_version: target.to_owned(),
            goal_complete,
            fulfilled,
        }
    }

    fn state_with(current: &str, milestones: Vec<crate::state::Milestone>) -> ProjectState {
        ProjectState {
            current_version: SemVer::parse(current).expect("version"),
            milestones,
            ..ProjectState::default()
        }
    }

    fn feature_at(id: &str, status: Status) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            format!("feature {id}"),
            "fixture",
            Priority::Medium,
            Complexity::Medium,
            false,
        )
        .expect("ticket");
        if status == Status::OnHold {
            t.transition_to(Role::Po, Status::OnHold).expect("hold");
            return t;
        }
        if status == Status::Rejected {
            t.transition_to(Role::Po, Status::Rejected).expect("reject");
            return t;
        }
        if status != Status::Pending {
            t.set_technical_design(Role::Sa, TechnicalDesign::default())
                .expect("attach design");
            t.transition_to(Role::Sa, Status::Ready).expect("ready");
            if matches!(
                status,
                Status::InProgress | Status::Done | Status::Documented
            ) {
                t.transition_to(Role::DevFeature, Status::InProgress)
                    .expect("claim");
            }
            if matches!(status, Status::Done | Status::Documented) {
                t.transition_to(Role::DevFeature, Status::Done)
                    .expect("done");
            }
            if status == Status::Documented {
                t.transition_to(Role::Docs, Status::Documented)
                    .expect("documented");
            }
        }
        t
    }

    /// A bug walked to `status` along the only legal bug edges (`Ticket::new`
    /// starts bugs in `Open`; `Fixed` is DEV-BUG's edge; `Verified` is TEST's).
    fn bug_at(id: &str, status: Status) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Bug,
            format!("bug {id}"),
            "fixture",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("bug");
        if status == Status::Rejected {
            t.transition_to(Role::Po, Status::Rejected).expect("reject");
            return t;
        }
        if matches!(
            status,
            Status::InProgress | Status::Fixed | Status::Verified
        ) {
            t.transition_to(Role::DevBug, Status::InProgress)
                .expect("claim");
        }
        if matches!(status, Status::Fixed | Status::Verified) {
            t.transition_to(Role::DevBug, Status::Fixed).expect("fixed");
        }
        if status == Status::Verified {
            t.transition_to(Role::Test, Status::Verified)
                .expect("verified");
        }
        t
    }

    fn scrum(committed: &[&str]) -> Sprint {
        Sprint {
            number: 1,
            goal: String::new(),
            started_cycle: 0,
            length_cycles: 10,
            committed: committed
                .iter()
                .copied()
                .filter_map(|c| TicketId::new(c).ok())
                .collect(),
            started_at: String::new(),
            bug_burn_floor: None,
        }
    }

    // --- classification branches (the design's test plan) -------------------

    #[test]
    fn fulfilled_reads_released_regardless_of_version() {
        // A fulfilled milestone below an unreachable target stays released:
        // the pipeline's own record wins over version arithmetic.
        let state = state_with("0.1.0", vec![milestone("Ancient", "9.9.9", false, true)]);
        let rows = project_milestones(&state);
        assert!(rows[0].released, "fulfilled -> released: {rows:?}");
        assert!(rows[0].reached, "released is definitionally reached");
    }

    #[test]
    fn version_at_or_above_target_reads_reached_when_not_fulfilled() {
        // Scope done, version there, release not yet tagged: the pipeline's
        // pending-release state, visible as reached without released.
        let state = state_with("0.5.2", vec![milestone("Beta", "0.5.0", true, false)]);
        let rows = project_milestones(&state);
        assert!(!rows[0].released);
        assert!(rows[0].reached, "current >= target -> reached: {rows:?}");
        assert!(rows[0].goal_complete);
    }

    #[test]
    fn scope_done_below_target_is_the_ready_to_ship_row() {
        let state = state_with("0.5.2", vec![milestone("Beta", "0.9.0", true, false)]);
        let rows = project_milestones(&state);
        assert!(!rows[0].released);
        assert!(
            !rows[0].reached,
            "current < target is never reached: {rows:?}"
        );
        assert!(rows[0].goal_complete, "the ready-to-ship marker");
    }

    #[test]
    fn active_target_is_the_first_unreleased_row_and_later_ones_follow() {
        let state = state_with(
            "0.5.2",
            vec![
                milestone("Beta", "0.9.0", false, false),
                milestone("GA", "1.0.0", false, false),
            ],
        );
        let rows = project_milestones(&state);
        let targets: Vec<&str> = rows.iter().map(|r| r.target_version.as_str()).collect();
        assert_eq!(
            targets,
            vec!["0.9.0", "1.0.0"],
            "declared (ascending) order: 0.9.0 is the active target, 1.0.0 ahead"
        );
    }

    #[test]
    fn malformed_target_never_reads_released_or_reached() {
        let state = state_with("0.5.2", vec![milestone("Broken", "soon", false, false)]);
        let rows = project_milestones(&state);
        assert!(!rows[0].released);
        assert!(
            !rows[0].reached,
            "an unparseable target must not read as done"
        );
        // ...but a FULFILLED milestone needs no parseable target: the
        // release gate checks fulfilled before it ever parses.
        let done = state_with("0.5.2", vec![milestone("Shipped", "soon", true, true)]);
        assert!(project_milestones(&done)[0].released);
    }

    #[test]
    fn empty_roadmap_projects_no_rows() {
        assert!(project_milestones(&ProjectState::default()).is_empty());
    }

    // --- CXA-F252: derived ticket→milestone attribution ----------------------

    #[test]
    fn committed_scope_attributes_to_the_active_target_row_only() {
        let mut state = state_with(
            "0.5.2",
            vec![
                milestone("Shipped", "0.5.0", true, true),
                milestone("Beta", "0.9.0", false, false),
                milestone("GA", "1.0.0", false, false),
            ],
        );
        state.tickets = vec![
            feature_at("FEAT-A", Status::InProgress),
            feature_at("FEAT-B", Status::Ready),
        ];
        state.sprint = Some(scrum(&["FEAT-A", "FEAT-B"]));

        // The derived rule: a ticket belongs to the first unreleased
        // milestone iff it is in the active sprint's committed set.
        assert_eq!(
            attributed_milestone(&state, &TicketId::new("FEAT-A").expect("id")),
            Some("Beta".to_owned()),
            "a committed ticket attributes to the active target"
        );

        let rows = project_milestones(&state);
        let scope: Vec<_> = rows.iter().map(|r| r.committed.len()).collect();
        assert_eq!(
            scope,
            vec![0, 2, 0],
            "only the active target row carries committed scope: {scope:?}"
        );
        assert_eq!(
            rows[1].committed,
            vec![
                TicketId::new("FEAT-A").expect("id"),
                TicketId::new("FEAT-B").expect("id")
            ],
            "commit order is preserved"
        );
        assert_eq!(rows[1].open, rows[1].committed, "nothing delivered yet");
        assert_eq!(rows[1].id, "Beta");
        assert_eq!(rows[1].id, rows[1].name, "the id IS the name — no new id");
        assert!(rows[0].open.is_empty() && rows[2].open.is_empty());
    }

    #[test]
    fn open_scope_closes_only_delivered_or_rejected_work() {
        let mut state = state_with("0.5.2", vec![milestone("Beta", "0.9.0", false, false)]);
        state.tickets = vec![
            feature_at("FEAT-DONE", Status::Done),
            feature_at("FEAT-DOC", Status::Documented),
            feature_at("FEAT-PROG", Status::InProgress),
            feature_at("FEAT-HOLD", Status::OnHold),
            bug_at("BUG-VER", Status::Verified),
            bug_at("BUG-GATE", Status::Fixed),
            bug_at("BUG-REJ", Status::Rejected),
        ];
        state.sprint = Some(scrum(&[
            "FEAT-DONE",
            "FEAT-DOC",
            "FEAT-PROG",
            "FEAT-HOLD",
            "BUG-VER",
            "BUG-GATE",
            "BUG-REJ",
        ]));

        let rows = project_milestones(&state);
        let open: Vec<_> = rows[0].open.iter().map(TicketId::as_str).collect();
        assert_eq!(
            open,
            vec!["FEAT-PROG", "FEAT-HOLD", "BUG-GATE"],
            "delivered and rejected work closes; in-flight, parked and \
             fixed-awaiting-human-verification work stays open: {open:?}"
        );
        assert_eq!(
            rows[0].committed.len(),
            7,
            "committed scope is the whole commit, open is its undelivered subset"
        );
    }

    #[test]
    fn kanban_and_uncommitted_tickets_attribute_to_nothing() {
        let mut state = state_with("0.5.2", vec![milestone("Beta", "0.9.0", false, false)]);
        state.tickets = vec![feature_at("FEAT-A", Status::Ready)];
        // Kanban: no sprint, no committed set — nothing to attribute through.
        assert_eq!(
            attributed_milestone(&state, &TicketId::new("FEAT-A").expect("id")),
            None,
            "Kanban has no committed scope to attribute"
        );
        // Scrum that committed OTHER work: FEAT-A stays unattributed…
        state.sprint = Some(scrum(&["FEAT-B"]));
        assert_eq!(
            attributed_milestone(&state, &TicketId::new("FEAT-A").expect("id")),
            None
        );
        // …and the row carries no scope: the commit never named it.
        let rows = project_milestones(&state);
        assert!(rows[0].committed.is_empty() && rows[0].open.is_empty());
    }

    #[test]
    fn absent_or_parse_broken_roadmaps_yield_no_attribution() {
        // No roadmap at all: committed work has nowhere to go.
        let state = ProjectState {
            current_version: SemVer::parse("0.5.2").expect("version"),
            tickets: vec![feature_at("FEAT-A", Status::Ready)],
            sprint: Some(scrum(&["FEAT-A"])),
            ..ProjectState::default()
        };
        assert_eq!(
            attributed_milestone(&state, &TicketId::new("FEAT-A").expect("id")),
            None,
            "no roadmap, no attribution"
        );
        assert!(project_milestones(&state).is_empty());

        // Parse-broken roadmap: the active target's version is unparseable,
        // so the release pipeline could never fulfill it — attribution
        // withholds rather than park scope on a row that cannot complete.
        let broken = ProjectState {
            tickets: vec![feature_at("FEAT-A", Status::Ready)],
            sprint: Some(scrum(&["FEAT-A"])),
            ..state_with("0.5.2", vec![milestone("Broken", "soon", false, false)])
        };
        assert_eq!(
            attributed_milestone(&broken, &TicketId::new("FEAT-A").expect("id")),
            None,
            "a parse-broken roadmap attributes nothing"
        );
        let rows = project_milestones(&broken);
        assert!(!rows[0].released && !rows[0].reached, "still classified");
        assert!(
            rows[0].committed.is_empty() && rows[0].open.is_empty(),
            "the broken row never receives scope"
        );

        // A FULFILLED row with an unparseable target does NOT block
        // attribution: the release gate checks fulfilled before it parses.
        let mixed = ProjectState {
            tickets: vec![feature_at("FEAT-A", Status::Ready)],
            sprint: Some(scrum(&["FEAT-A"])),
            ..state_with(
                "0.5.2",
                vec![
                    milestone("Ancient", "soon", true, true),
                    milestone("Beta", "0.9.0", false, false),
                ],
            )
        };
        assert_eq!(
            attributed_milestone(&mixed, &TicketId::new("FEAT-A").expect("id")),
            Some("Beta".to_owned())
        );
    }

    #[test]
    fn a_fully_shipped_roadmap_attributes_to_nothing() {
        // Every row released: no unreleased milestone exists to own the
        // sprint's committed scope, so attribution withholds entirely.
        let state = ProjectState {
            tickets: vec![feature_at("FEAT-A", Status::Ready)],
            sprint: Some(scrum(&["FEAT-A"])),
            ..state_with("0.5.2", vec![milestone("Shipped", "0.5.0", true, true)])
        };
        assert_eq!(
            attributed_milestone(&state, &TicketId::new("FEAT-A").expect("id")),
            None,
            "all rows released — no active target to attribute to"
        );
        let rows = project_milestones(&state);
        assert!(rows[0].committed.is_empty() && rows[0].open.is_empty());
        assert!(rows[0].released, "the row still classifies as released");
    }

    #[test]
    fn stale_committed_ids_never_haunt_the_milestone_row() {
        let mut state = state_with("0.5.2", vec![milestone("Beta", "0.9.0", false, false)]);
        state.tickets = vec![feature_at("FEAT-A", Status::InProgress)];
        // The commit names a ticket that no longer exists on the board.
        state.sprint = Some(scrum(&["FEAT-A", "FEAT-GONE"]));
        let rows = project_milestones(&state);
        assert_eq!(
            rows[0]
                .committed
                .iter()
                .map(TicketId::as_str)
                .collect::<Vec<_>>(),
            vec!["FEAT-A"],
            "committed scope lists tickets that still exist, in commit order"
        );
        assert!(rows[0].open.contains(&TicketId::new("FEAT-A").expect("id")));
    }

    // --- blocked-scope surfacing through the same selection predicates ------

    #[test]
    fn blocked_tickets_come_from_the_selection_predicates() {
        let mut blocked = feature_at("FEAT-B", Status::Ready);
        blocked
            .add_dependency(Role::Sa, TicketId::new("BUG-1").expect("id"))
            .expect("dep");
        let bug = Ticket::new(
            TicketId::new("BUG-1").expect("id"),
            TicketType::Bug,
            "the blocker",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("ticket");

        // Kanban (no sprint): the feature is in scope, so the only reason it
        // cannot move is its unsatisfied dependency.
        let kanban = ProjectState {
            current_version: SemVer::parse("0.5.2").expect("version"),
            tickets: vec![bug.clone(), blocked.clone()],
            ..ProjectState::default()
        };
        let kanban_report = projection_report(&kanban);
        let reasons: Vec<_> = kanban_report
            .blocked_tickets
            .iter()
            .map(|b| (b.id.as_str(), b.reason))
            .collect();
        assert_eq!(
            reasons,
            vec![("FEAT-B", selection::BlockedReason::DependencyUnsatisfied)],
            "Kanban yields the dependency reason only: {reasons:?}"
        );

        // Scrum without a commit: the same feature is ALSO out of scope.
        let scrum_uncommitted = ProjectState {
            current_version: SemVer::parse("0.5.2").expect("version"),
            tickets: vec![bug, blocked],
            sprint: Some(scrum(&[])),
            ..ProjectState::default()
        };
        let scrum_report = projection_report(&scrum_uncommitted);
        let reasons: Vec<_> = scrum_report
            .blocked_tickets
            .iter()
            .map(|b| (b.id.as_str(), b.reason))
            .collect();
        assert_eq!(
            reasons,
            vec![
                ("FEAT-B", selection::BlockedReason::DependencyUnsatisfied),
                ("FEAT-B", selection::BlockedReason::OutOfDevScope),
            ],
            "a ticket can be blocked for two reasons at once: {reasons:?}"
        );
    }

    // --- purity and the wire contract ---------------------------------------

    #[test]
    fn projection_is_a_pure_deterministic_read() {
        let mut state = state_with("0.5.2", vec![milestone("Beta", "0.9.0", false, false)]);
        state.tickets.push(feature_at("FEAT-A", Status::Pending));
        // A commit over delivered + in-flight work: the scope half of the
        // read model runs too, not just the classification half.
        state.tickets.push(feature_at("FEAT-B", Status::Done));
        state.sprint = Some(scrum(&["FEAT-A", "FEAT-B"]));
        let before = state.clone();

        let first = projection_report(&state);
        assert_eq!(state, before, "the read never mutates the state it reads");
        assert_eq!(
            projection_report(&state),
            first,
            "the same snapshot always yields the same report"
        );
        assert_eq!(
            first.milestones[0].open,
            vec![TicketId::new("FEAT-A").expect("id")],
            "the deterministic read carries the derived scope"
        );
    }

    #[test]
    fn wire_shape_is_exactly_the_contract() {
        let mut state = state_with("0.5.2", vec![milestone("Beta", "0.9.0", false, false)]);
        state.tickets.push(feature_at("FEAT-A", Status::Pending));
        // An open Scrum sprint that did not commit FEAT-A gives the payload a
        // blocked row to pin.
        state.sprint = Some(scrum(&[]));
        let v = serde_json::to_value(projection_report(&state)).expect("serialize");

        // Key SETS, not orderings: the contract is the set of fields — the
        // exact set pins that there is NO date field to fabricate a date into.
        let top: std::collections::BTreeSet<_> =
            v.as_object().expect("object").keys().cloned().collect();
        assert_eq!(
            top,
            std::collections::BTreeSet::from([
                "blocked_tickets".to_owned(),
                "current_version".to_owned(),
                "milestones".to_owned(),
            ]),
            "the payload is exactly the contracted keys — in particular there is \
             NO date field, so the projection can never fabricate a date"
        );
        assert_eq!(v["current_version"], "0.5.2");

        let row = &v["milestones"][0];
        let keys: std::collections::BTreeSet<_> =
            row.as_object().expect("object").keys().cloned().collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "committed".to_owned(),
                "goal_complete".to_owned(),
                "id".to_owned(),
                "name".to_owned(),
                "open".to_owned(),
                "progress".to_owned(),
                "reached".to_owned(),
                "released".to_owned(),
                "target_version".to_owned(),
            ]),
            "F249's exact keys kept, plus the CXA-F252 trio (id + committed/open scope) \
             and CXA-F361's progress figure"
        );
        assert_eq!(row["id"], row["name"], "the wire id IS the name (CXA-F252)");
        assert_eq!(
            row["committed"],
            serde_json::json!([]),
            "the sprint committed nothing, so the active row carries no scope"
        );
        assert_eq!(row["open"], serde_json::json!([]));
        assert_eq!(
            row["progress"], 0,
            "no attributed scope reads 0 — never NaN, never invented (CXA-F361)"
        );

        let blocked = &v["blocked_tickets"][0];
        assert_eq!(blocked["id"], "FEAT-A");
        assert_eq!(blocked["title"], "feature FEAT-A");
        assert_eq!(blocked["reason"], "out-of-dev-scope");
    }

    #[test]
    fn wire_scope_is_strings_and_excludes_closed_work() {
        let mut state = state_with("0.5.2", vec![milestone("Beta", "0.9.0", false, false)]);
        state.tickets = vec![
            feature_at("FEAT-A", Status::InProgress),
            feature_at("FEAT-B", Status::Done),
        ];
        state.sprint = Some(scrum(&["FEAT-A", "FEAT-B"]));
        let v = serde_json::to_value(projection_report(&state)).expect("serialize");
        let row = &v["milestones"][0];
        assert_eq!(
            row["committed"],
            serde_json::json!(["FEAT-A", "FEAT-B"]),
            "ticket ids land on the wire as plain strings"
        );
        assert_eq!(
            row["open"],
            serde_json::json!(["FEAT-A"]),
            "the delivered ticket is committed scope but no longer open"
        );
    }
}
