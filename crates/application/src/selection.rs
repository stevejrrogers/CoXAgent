//! Pure ticket-selection policy. The orchestrator asks "what's next?"; these
//! functions answer deterministically from state, so the choice is testable
//! without any engine. Kept separate from the loop for the same reason.

use crate::state::ProjectState;
use coxagent_domain::{Priority, Status, Ticket, TicketId, TicketType};

/// True when an active bug-burn floor excludes priority `p` (CXA-F028): the
/// burn targets bugs AT OR ABOVE the floor, so anything strictly below is
/// parked for the burn's duration. `None` = no burn floor — every open bug
/// burns, exactly as before the floor existed.
#[must_use]
pub(crate) fn below_bug_burn_floor(floor: Option<Priority>, p: Priority) -> bool {
    floor.is_some_and(|f| p < f)
}

/// The sprint's active bug-burn floor, or `None` outside a burn (no sprint /
/// a sprint opened before the floor existed).
fn active_bug_burn_floor(state: &ProjectState) -> Option<Priority> {
    state.sprint.as_ref().and_then(|s| s.bug_burn_floor)
}

/// Open bugs, best-first (priority desc, id asc). The `*_candidates` variants
/// return the whole ordered queue so a runner that loses a claim race can fall
/// through to the next ticket instead of idling — the basis for two runners
/// picking *different* tickets and working in parallel. Under an active
/// bug-burn floor, bugs below the floor are excluded: the burn burns the
/// right severities (CXA-F028).
#[must_use]
pub fn open_bug_candidates(state: &ProjectState) -> Vec<TicketId> {
    let floor = active_bug_burn_floor(state);
    candidates(state, |t| {
        t.ticket_type() == TicketType::Bug
            && t.status() == Status::Open
            && !below_bug_burn_floor(floor, t.priority())
    })
}

/// Highest-priority open bug (critical work first).
#[must_use]
pub fn next_open_bug(state: &ProjectState) -> Option<TicketId> {
    open_bug_candidates(state).into_iter().next()
}

/// `pending` feature/chore tickets still missing a technical design (SA queue).
#[must_use]
pub fn design_candidates(state: &ProjectState) -> Vec<TicketId> {
    candidates(state, |t| {
        matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
            && t.status() == Status::Pending
            && t.design().technical.is_none()
    })
}

/// Highest-priority feature/chore still missing its technical design.
#[must_use]
pub fn next_feature_needing_design(state: &ProjectState) -> Option<TicketId> {
    design_candidates(state).into_iter().next()
}

/// `pending` UI feature/chore tickets that have a technical design but still
/// need UX (PD queue).
#[must_use]
pub fn ux_candidates(state: &ProjectState) -> Vec<TicketId> {
    candidates(state, |t| {
        matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
            && t.status() == Status::Pending
            && t.has_ui()
            && t.design().technical.is_some()
            && t.design().ux.is_none()
    })
}

/// Highest-priority `pending` UI feature/chore that still needs UX.
#[must_use]
pub fn next_feature_needing_ux(state: &ProjectState) -> Option<TicketId> {
    ux_candidates(state).into_iter().next()
}

/// Feature/chore tickets in `Done` awaiting documentation (DOCS queue).
#[must_use]
pub fn documentable_candidates(state: &ProjectState) -> Vec<TicketId> {
    candidates(state, |t| {
        matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
            && t.status() == Status::Done
    })
}

/// Highest-priority feature/chore in `Done` awaiting documentation.
#[must_use]
pub fn next_documentable(state: &ProjectState) -> Option<TicketId> {
    documentable_candidates(state).into_iter().next()
}

/// `ready` features whose dependencies are all satisfied (DEV queue).
#[must_use]
pub fn ready_feature_candidates(state: &ProjectState) -> Vec<TicketId> {
    candidates(state, |t| {
        matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
            && t.status() == Status::Ready
            && deps_satisfied(state, t)
    })
}

/// Highest-priority `ready` feature whose dependencies are all satisfied.
#[must_use]
pub fn next_ready_feature(state: &ProjectState) -> Option<TicketId> {
    ready_feature_candidates(state).into_iter().next()
}

/// A feature/chore is workable only when every dependency has reached `Done`
/// (or beyond). Prevents handing out a ticket blocked by unfinished work.
fn deps_satisfied(state: &ProjectState, ticket: &Ticket) -> bool {
    ticket.depends_on().iter().all(|dep| {
        state
            .ticket(dep)
            .is_some_and(|d| matches!(d.status(), Status::Done | Status::Documented))
    })
}

/// All tickets matching `pred`, priority-ordered: tickets committed to the
/// current sprint first (the team's aligned work), then the rest best-first.
/// Within each scope bucket: priority desc, then id ascending.
fn candidates<F: Fn(&Ticket) -> bool>(state: &ProjectState, pred: F) -> Vec<TicketId> {
    // An assignee records WHO owns a ticket, it does not LOCK it out of the
    // team's work. In this app the running agents are the operator's team, so
    // a ticket assigned to the operator is prioritised work for them, not a
    // walled-off lane — a human-assigned ticket can still be actioned by the
    // team (the assignee flag is an ownership label, not a scheduler block).
    let mut matched: Vec<&Ticket> = state.tickets.iter().filter(|t| pred(t)).collect();
    matched.sort_by(|a, b| {
        let a_in = in_dev_scope(state, a.id());
        let b_in = in_dev_scope(state, b.id());
        b_in.cmp(&a_in) // in-scope first
            .then_with(|| {
                priority_rank(b.priority())
                    .cmp(&priority_rank(a.priority()))
                    .then_with(|| a.id().as_str().cmp(b.id().as_str()))
            })
    });
    matched.into_iter().map(|t| t.id().clone()).collect()
}

fn priority_rank(p: Priority) -> u8 {
    match p {
        Priority::Low => 0,
        Priority::Medium => 1,
        Priority::High => 2,
    }
}

/// Is `id` inside the DEV work scope for the current sprint?
///
/// Real-world rule: DEV only pulls tickets the team committed to this sprint
/// (PO/SM aligned via the sprint-board action). Three things shape that:
///  - bugs still `Open` are emergency work that outranks the board — UNLESS an
///    active bug-burn floor scopes the burn above their severity (CXA-F028):
///    a below-floor Open bug is parked for the burn's duration, while a bug
///    at/above the floor stays workable even mid-burn (a genuine critical is
///    never blocked),
///  - Kanban mode (no sprint open) — there is no sprint to be out of scope
///    for, so any ready ticket is fair game, and
///  - Scrum mode: a feature/chore the PO/SM has not committed is out of scope;
///    DEV must ask to have it added before picking it up.
#[must_use]
pub fn in_dev_scope(state: &ProjectState, id: &TicketId) -> bool {
    // Emergency bugs are always workable, sprint or not — above the burn floor.
    if let Some(t) = state.ticket(id) {
        if matches!(
            (t.ticket_type(), t.status()),
            (TicketType::Bug, Status::Open)
        ) {
            let floor = active_bug_burn_floor(state);
            return !below_bug_burn_floor(floor, t.priority());
        }
    }
    // Kanban mode (no sprint open): no scope ceremony — DEV may pull any
    // ready ticket.
    let Some(sprint) = state.sprint.as_ref() else {
        return true;
    };
    // Scrum mode: features/chores only get worked when the team committed
    // them to this sprint (PO/SM aligned via the sprint-board action).
    sprint.committed.contains(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Complexity, Role, TechnicalDesign};

    fn ready_feature(id: &str, prio: Priority) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            prio,
            Complexity::Small,
            false,
        )
        .expect("ticket");
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("design");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t
    }

    #[test]
    fn picks_highest_priority_ready_feature() {
        let state = ProjectState {
            tickets: vec![
                ready_feature("FEAT-001", Priority::Low),
                ready_feature("FEAT-002", Priority::High),
                ready_feature("FEAT-003", Priority::Medium),
            ],
            ..ProjectState::default()
        };
        assert_eq!(
            next_ready_feature(&state).expect("some").as_str(),
            "FEAT-002"
        );
    }

    #[test]
    fn skips_feature_with_unfinished_dependency() {
        let mut blocked = ready_feature("FEAT-002", Priority::High);
        blocked
            .add_dependency(Role::Sa, TicketId::new("FEAT-001").expect("id"))
            .expect("dep");
        let state = ProjectState {
            // FEAT-001 is only ready (not done), so FEAT-002 must be skipped.
            tickets: vec![ready_feature("FEAT-001", Priority::Low), blocked],
            ..ProjectState::default()
        };
        assert_eq!(
            next_ready_feature(&state).expect("some").as_str(),
            "FEAT-001"
        );
    }

    #[test]
    fn no_ready_feature_returns_none() {
        assert!(next_ready_feature(&ProjectState::default()).is_none());
    }

    // Ticket::new creates bugs already in `Open`.
    fn open_bug(id: &str) -> Ticket {
        open_bug_prio(id, Priority::High)
    }

    fn open_bug_prio(id: &str, prio: Priority) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Bug,
            "b",
            "",
            prio,
            Complexity::Small,
            false,
        )
        .expect("ticket")
    }

    fn sprint(committed: &[&str]) -> crate::state::Sprint {
        sprint_with_floor(committed, None)
    }

    fn sprint_with_floor(committed: &[&str], floor: Option<Priority>) -> crate::state::Sprint {
        crate::state::Sprint {
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
            bug_burn_floor: floor,
        }
    }

    /// A `pending` feature with no technical design yet (the SA queue).
    fn pending_feature_needing_design(id: &str, prio: Priority) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            prio,
            Complexity::Small,
            false,
        )
        .expect("ticket")
    }

    #[test]
    fn design_prefers_committed_feature_over_out_of_scope() {
        // Same priority; the committed one must be picked first even though the
        // out-of-scope one comes lexicographically earlier.
        let state = ProjectState {
            tickets: vec![
                pending_feature_needing_design("CXA-F010", Priority::High),
                pending_feature_needing_design("CXA-F003", Priority::High),
            ],
            sprint: Some(sprint(&["CXA-F010"])),
            ..ProjectState::default()
        };
        assert_eq!(
            next_feature_needing_design(&state).expect("some").as_str(),
            "CXA-F010"
        );
    }

    #[test]
    fn design_falls_back_to_out_of_scope_when_no_committed_work() {
        // No committed feature needs design but an out-of-scope one does — it
        // is still designed so the pipeline isn't starved (the sprint cost-free
        // fallback; DEV only ignores it because it isn't scoped).
        let state = ProjectState {
            tickets: vec![pending_feature_needing_design("CXA-F020", Priority::High)],
            sprint: Some(sprint(&["CXA-F011"])),
            ..ProjectState::default()
        };
        assert_eq!(
            next_feature_needing_design(&state).expect("some").as_str(),
            "CXA-F020"
        );
    }

    #[test]
    fn in_scope_committed_feature_is_workable() {
        let state = ProjectState {
            tickets: vec![ready_feature("CXA-F001", Priority::High)],
            sprint: Some(sprint(&["CXA-F001"])),
            ..ProjectState::default()
        };
        assert!(in_dev_scope(
            &state,
            &TicketId::new("CXA-F001").expect("id")
        ));
    }

    #[test]
    fn out_of_scope_ready_feature_is_blocked() {
        let state = ProjectState {
            tickets: vec![ready_feature("CXA-F023", Priority::High)],
            // Sprint committed something else; F023 was never PO/SM-aligned.
            sprint: Some(sprint(&["CXA-F004"])),
            ..ProjectState::default()
        };
        assert!(!in_dev_scope(
            &state,
            &TicketId::new("CXA-F023").expect("id")
        ));
    }

    #[test]
    fn open_bug_is_always_in_scope() {
        let state = ProjectState {
            tickets: vec![open_bug("CXA-B002")],
            ..ProjectState::default()
        };
        // No sprint at all — still workable because it is an emergency bug.
        assert!(in_dev_scope(
            &state,
            &TicketId::new("CXA-B002").expect("id")
        ));
    }

    // ---- CXA-F028: an active bug-burn floor scopes the DEV bug queue. ----

    #[test]
    fn burn_floor_excludes_below_floor_bugs_and_keeps_best_first_order() {
        let state = ProjectState {
            tickets: vec![
                open_bug_prio("CXA-B-LOW", Priority::Low),
                open_bug_prio("CXA-B-MED", Priority::Medium),
                open_bug_prio("CXA-B-HIGH", Priority::High),
            ],
            sprint: Some(sprint_with_floor(&[], Some(Priority::Medium))),
            ..ProjectState::default()
        };
        // Only at/above-floor bugs burn, best-first (priority desc).
        assert_eq!(
            open_bug_candidates(&state),
            vec![
                TicketId::new("CXA-B-HIGH").expect("id"),
                TicketId::new("CXA-B-MED").expect("id"),
            ]
        );
        // The same sprint without a floor keeps every open bug queued.
        let state = ProjectState {
            tickets: state.tickets,
            sprint: Some(sprint(&[])),
            ..ProjectState::default()
        };
        assert_eq!(open_bug_candidates(&state).len(), 3);
    }

    #[test]
    fn mid_burn_criticals_stay_in_scope() {
        let state = ProjectState {
            tickets: vec![
                open_bug_prio("CXA-B-CRIT", Priority::High),
                open_bug_prio("CXA-B-MED", Priority::Medium),
            ],
            // An active High-floor burn: the emergency clause still covers
            // bugs at/above the floor — a genuine critical is never blocked.
            sprint: Some(sprint_with_floor(&[], Some(Priority::High))),
            ..ProjectState::default()
        };
        assert!(in_dev_scope(
            &state,
            &TicketId::new("CXA-B-CRIT").expect("id")
        ));
        assert!(!in_dev_scope(
            &state,
            &TicketId::new("CXA-B-MED").expect("id")
        ));
    }

    #[test]
    fn below_floor_open_bugs_are_out_of_scope_mid_burn_even_if_committed() {
        let state = ProjectState {
            tickets: vec![open_bug_prio("CXA-B-LOW", Priority::Low)],
            // The bug somehow sits on the sprint (e.g. committed before the
            // burn began): the floor still parks it — DEV must not work it.
            sprint: Some(sprint_with_floor(&["CXA-B-LOW"], Some(Priority::High))),
            ..ProjectState::default()
        };
        assert!(!in_dev_scope(
            &state,
            &TicketId::new("CXA-B-LOW").expect("id")
        ));
    }

    #[test]
    fn no_burn_window_means_open_bugs_stay_in_scope() {
        let low = open_bug_prio("CXA-B-LOW", Priority::Low);
        // No sprint at all (Kanban): nothing scopes the burn.
        let state = ProjectState {
            tickets: vec![low.clone()],
            ..ProjectState::default()
        };
        assert!(in_dev_scope(&state, low.id()));
        // A sprint WITHOUT a floor (old snapshot / floor unset): unchanged.
        let state = ProjectState {
            tickets: vec![low.clone()],
            sprint: Some(sprint(&[])),
            ..ProjectState::default()
        };
        assert!(in_dev_scope(&state, low.id()));
    }

    #[test]
    fn no_sprint_means_kanban_features_are_in_scope() {
        let state = ProjectState {
            tickets: vec![ready_feature("CXA-F001", Priority::High)],
            ..ProjectState::default()
        };
        // Kanban mode (no sprint): no ceremony, any ready feature is workable.
        assert!(in_dev_scope(
            &state,
            &TicketId::new("CXA-F001").expect("id")
        ));
    }

    #[test]
    fn committed_feature_is_in_scope_under_sprint() {
        let state = ProjectState {
            tickets: vec![ready_feature("CXA-F001", Priority::High)],
            sprint: Some(sprint(&["CXA-F001"])),
            ..ProjectState::default()
        };
        assert!(in_dev_scope(
            &state,
            &TicketId::new("CXA-F001").expect("id")
        ));
    }
}
