//! Forward milestone-completion projection (CXA-F249) — a pure read model over
//! `ProjectState`: where each declared milestone stands against the current
//! version, and which open work cannot move right now.
//!
//! No IO, no mutation, no fabrication. There is no ticket↔milestone
//! attribution anywhere in the persisted model, so a per-milestone date or
//! remaining-count would be invented data; instead the projection classifies
//! milestones through EXACTLY the gates the release pipeline applies
//! (`RunReleasesUseCase`: fulfilled -> released; current_version >= parsed
//! target) so this view can never contradict what the pipeline will do next,
//! and surfaces blocked work through the same selection predicates the
//! scheduler uses (`selection::work_blockers`). "Insufficient data" is
//! structural: the read model has no date field at all, so it can never
//! fabricate one.

use serde::Serialize;

use crate::selection::{self, BlockedWork};
use crate::state::ProjectState;
use coxagent_domain::SemVer;

/// One milestone's forward read model, classified purely from state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilestoneProjection {
    pub name: String,
    pub target_version: String,
    pub goal_complete: bool,
    /// The release pipeline already tagged this milestone (`fulfilled`).
    pub released: bool,
    /// The current version has reached the target — or the milestone is
    /// released, which is definitionally reached.
    pub reached: bool,
}

/// The whole `GET /api/projects/:pid/milestones/projection` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilestoneProjectionReport {
    pub current_version: String,
    pub milestones: Vec<MilestoneProjection>,
    pub blocked_tickets: Vec<BlockedWork>,
}

/// Classify every declared milestone, keeping the PO-authored declared order
/// (ascending targets, enforced by milestone planning): the first unreleased
/// row is the active target the sprints are working toward, the unreleased
/// rows after it are ahead. A malformed target never reads as released or
/// reached — a broken roadmap must not masquerade as done work; an already
/// `fulfilled` milestone reads released regardless of target parseability
/// (the release gate checks `fulfilled` first, before it ever parses).
#[must_use]
pub fn project_milestones(state: &ProjectState) -> Vec<MilestoneProjection> {
    state
        .milestones
        .iter()
        .map(|m| {
            let released = m.fulfilled;
            let target = SemVer::parse(&m.target_version).ok();
            let reached = released || target.is_some_and(|t| state.current_version >= t);
            MilestoneProjection {
                name: m.name.clone(),
                target_version: m.target_version.clone(),
                goal_complete: m.goal_complete,
                released,
                reached,
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

    fn milestone(name: &str, target: &str, goal_complete: bool, fulfilled: bool) -> crate::state::Milestone {
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
        let state = state_with(
            "0.1.0",
            vec![milestone("Ancient", "9.9.9", false, true)],
        );
        let rows = project_milestones(&state);
        assert!(rows[0].released, "fulfilled -> released: {rows:?}");
        assert!(rows[0].reached, "released is definitionally reached");
    }

    #[test]
    fn version_at_or_above_target_reads_reached_when_not_fulfilled() {
        // Scope done, version there, release not yet tagged: the pipeline's
        // pending-release state, visible as reached without released.
        let state = state_with(
            "0.5.2",
            vec![milestone("Beta", "0.5.0", true, false)],
        );
        let rows = project_milestones(&state);
        assert!(!rows[0].released);
        assert!(rows[0].reached, "current >= target -> reached: {rows:?}");
        assert!(rows[0].goal_complete);
    }

    #[test]
    fn scope_done_below_target_is_the_ready_to_ship_row() {
        let state = state_with(
            "0.5.2",
            vec![milestone("Beta", "0.9.0", true, false)],
        );
        let rows = project_milestones(&state);
        assert!(!rows[0].released);
        assert!(!rows[0].reached, "current < target is never reached: {rows:?}");
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
        let targets: Vec<&str> = rows
            .iter()
            .map(|r| r.target_version.as_str())
            .collect();
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
        assert!(!rows[0].reached, "an unparseable target must not read as done");
        // ...but a FULFILLED milestone needs no parseable target: the
        // release gate checks fulfilled before it ever parses.
        let done = state_with("0.5.2", vec![milestone("Shipped", "soon", true, true)]);
        assert!(project_milestones(&done)[0].released);
    }

    #[test]
    fn empty_roadmap_projects_no_rows() {
        assert!(project_milestones(&ProjectState::default()).is_empty());
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
        let mut state = state_with(
            "0.5.2",
            vec![milestone("Beta", "0.9.0", false, false)],
        );
        state.tickets.push(feature_at("FEAT-A", Status::Pending));
        let before = state.clone();

        let first = projection_report(&state);
        assert_eq!(state, before, "the read never mutates the state it reads");
        assert_eq!(
            projection_report(&state), first,
            "the same snapshot always yields the same report"
        );
    }

    #[test]
    fn wire_shape_is_exactly_the_contract() {
        let mut state = state_with(
            "0.5.2",
            vec![milestone("Beta", "0.9.0", false, false)],
        );
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
                "goal_complete".to_owned(),
                "name".to_owned(),
                "reached".to_owned(),
                "released".to_owned(),
                "target_version".to_owned(),
            ])
        );

        let blocked = &v["blocked_tickets"][0];
        assert_eq!(blocked["id"], "FEAT-A");
        assert_eq!(blocked["title"], "feature FEAT-A");
        assert_eq!(blocked["reason"], "out-of-dev-scope");
    }
}
