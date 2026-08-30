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

/// The next feature work: the first still-actionable (Pending/Ready)
/// feature/chore in backlog order — what a burn-down sprint is clearing the
/// road for. Bugs behind it in the backlog neither block nor gate it.
#[must_use]
pub fn next_feature_work(state: &ProjectState) -> Option<TicketId> {
    state
        .tickets
        .iter()
        .find(|t| {
            matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
                && matches!(t.status(), Status::Pending | Status::Ready)
        })
        .map(|t| t.id().clone())
}

/// The burn-down scope (CXA-F030): every OPEN bug that blocks the next
/// feature work (the feature's `depends_on` names it) or precedes it in the
/// backlog. These bugs stand between the team and its next feature, so a
/// sprint opened over them commits the scope IN FULL — never capacity-capped
/// (see `backlog_scoping::open_backlog`).
#[must_use]
pub fn burn_down_scope(state: &ProjectState) -> Vec<TicketId> {
    let Some(feature) = next_feature_work(state) else {
        return Vec::new();
    };
    let feature_pos = state.tickets.iter().position(|t| t.id() == &feature);
    let feature_deps = state.ticket(&feature).map(Ticket::depends_on);
    state
        .tickets
        .iter()
        .enumerate()
        .filter(|(_, t)| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
        .filter(|(pos, t)| {
            let blocks = feature_deps.is_some_and(|deps| deps.contains(t.id()));
            let precedes = feature_pos.is_some_and(|fp| *pos < fp);
            blocks || precedes
        })
        .map(|(_, t)| t.id().clone())
        .collect()
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
/// `Verified` counts too (CXA-F030): a fixed-and-verified bug is the
/// burn-down's terminal state — the clean baseline it leaves releases the
/// work it gated.
fn deps_satisfied(state: &ProjectState, ticket: &Ticket) -> bool {
    ticket.depends_on().iter().all(|dep| {
        state.ticket(dep).is_some_and(|d| {
            matches!(
                d.status(),
                Status::Done | Status::Documented | Status::Verified
            )
        })
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

/// Why a piece of open work cannot move (CXA-F249) — serialized kebab-case,
/// exactly the reason strings the projection endpoint surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockedReason {
    /// A `depends_on` ancestor has not reached Done/Documented/Verified —
    /// the same gate the scheduler applies before handing work out.
    DependencyUnsatisfied,
    /// Scrum mode and the PO/SM has not committed the ticket to the sprint
    /// (Kanban has no scope ceremony, so it never yields this reason).
    OutOfDevScope,
}

/// One open feature/chore the projection surfaces as blocked, with one entry
/// per distinct reason (a ticket blocked two ways appears twice, honestly).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BlockedWork {
    pub id: TicketId,
    pub title: String,
    pub reason: BlockedReason,
}

/// Open (Pending/Ready/InProgress) feature/chore work that cannot move, named
/// through the SAME predicates the scheduler uses — `deps_satisfied` and
/// `in_dev_scope` — so the projection can never disagree with what DEV is
/// actually allowed to pick up. Terminal states (Done/Documented/Verified),
/// rejected/parked tickets and bugs are not blockers on the path forward.
#[must_use]
pub fn work_blockers(state: &ProjectState) -> Vec<BlockedWork> {
    state
        .tickets
        .iter()
        .filter(|t| {
            matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
                && matches!(
                    t.status(),
                    Status::Pending | Status::Ready | Status::InProgress
                )
        })
        .flat_map(|t| {
            let mut reasons = Vec::new();
            if !deps_satisfied(state, t) {
                reasons.push(BlockedReason::DependencyUnsatisfied);
            }
            if !in_dev_scope(state, t.id()) {
                reasons.push(BlockedReason::OutOfDevScope);
            }
            reasons.into_iter().map(move |reason| BlockedWork {
                id: t.id().clone(),
                title: t.title().to_owned(),
                reason,
            })
        })
        .collect()
}

/// Whether the HUMAN burn mode (CXA-F030) currently holds DEV-FEATURE: the
/// mode is engaged and either no numeric exit gate was set — it then holds
/// until a person switches it off — or the open-bug count is still above the
/// target. The COUNT decides; priority order is irrelevant to the gate.
#[must_use]
pub fn burn_mode_holds(state: &ProjectState) -> bool {
    state.tuning.burn_mode
        && match state.tuning.burn_until_bugs_le {
            Some(target) => {
                let target = usize::try_from(target).unwrap_or(usize::MAX);
                open_bug_candidates(state).len() > target
            }
            None => true,
        }
}

/// Apply the burn mode's explicit exit gate to live state: an engaged mode
/// whose open-bug count has reached its target clears itself, so the
/// burn-down sprint ends without waiting for a person. Returns whether the
/// state changed — the caller persists it through the store.
pub fn clear_burn_mode_if_gate_met(state: &mut ProjectState) -> bool {
    if state.tuning.burn_mode && !burn_mode_holds(state) {
        state.tuning.burn_mode = false;
        true
    } else {
        false
    }
}

/// DEV-FEATURE's pause for one cycle: the reactive `bugs_first` brake and the
/// human burn mode hold independently — either one pauses features. Both
/// share the deadlock valve: a cycle whose bug slot produced nothing
/// (`bug_slot_worked == false`) never holds features, however loud the
/// brakes — the 50-cycle both-lanes-starved lesson from the reactive brake.
#[must_use]
pub fn dev_feature_paused(state: &ProjectState, bug_slot_worked: bool) -> bool {
    (state.tuning.bugs_first || burn_mode_holds(state)) && bug_slot_worked
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

    // --- CXA-F249: work_blockers names blocked work through both predicates ---

    fn blocked_feature(id: &str, blocker: &str) -> Ticket {
        let mut t = ready_feature(id, Priority::High);
        t.add_dependency(Role::Sa, TicketId::new(blocker).expect("id"))
            .expect("dep");
        t
    }

    #[test]
    fn work_blockers_names_the_dependency_reason_under_kanban() {
        let state = ProjectState {
            tickets: vec![open_bug("BUG-1"), blocked_feature("FEAT-1", "BUG-1")],
            ..ProjectState::default()
        };
        let rows = work_blockers(&state);
        let reasons: Vec<_> = rows
            .iter()
            .map(|b| (b.id.as_str(), b.reason))
            .collect();
        assert_eq!(
            reasons,
            vec![("FEAT-1", BlockedReason::DependencyUnsatisfied)],
            "Kanban: in scope, so the unsatisfied dep is the only reason: {reasons:?}"
        );
    }

    #[test]
    fn work_blockers_yields_nothing_for_committed_work_with_met_deps() {
        let state = ProjectState {
            tickets: vec![ready_feature("FEAT-1", Priority::High)],
            sprint: Some(sprint(&["FEAT-1"])),
            ..ProjectState::default()
        };
        assert!(
            work_blockers(&state).is_empty(),
            "committed, dependency-free work is not blocked"
        );
    }

    #[test]
    fn work_blockers_covers_every_in_play_status_not_just_ready() {
        // Pending needs no design to be blocked; InProgress is already
        // claimed. Uncommitted under an open sprint, both cannot move and
        // both must surface — the scan is over the work in play, not one
        // queue.
        let state = ProjectState {
            tickets: vec![
                Ticket::new(
                    TicketId::new("FEAT-P").expect("id"),
                    TicketType::Feature,
                    "pending",
                    "",
                    Priority::Medium,
                    Complexity::Small,
                    false,
                )
                .expect("ticket"),
                {
                    let mut t = ready_feature("FEAT-I", Priority::Medium);
                    t.transition_to(Role::DevFeature, Status::InProgress)
                        .expect("claim");
                    t
                },
            ],
            sprint: Some(sprint(&[])),
            ..ProjectState::default()
        };
        let rows = work_blockers(&state);
        let named: Vec<_> = rows.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(
            named,
            vec!["FEAT-P", "FEAT-I"],
            "pending and in-progress work both surface, declared order: {named:?}"
        );
        assert!(rows.iter().all(|b| b.reason == BlockedReason::OutOfDevScope));
    }

    // --- CXA-F030: the human burn mode and the burn-down scope ---

    fn tuning(burn_mode: bool, target: Option<u32>, bugs_first: bool) -> crate::state::Tuning {
        crate::state::Tuning {
            burn_mode,
            burn_until_bugs_le: target,
            bugs_first,
            ..crate::state::Tuning::default()
        }
    }

    fn state_with(t: crate::state::Tuning, bugs: &[&str]) -> ProjectState {
        ProjectState {
            tuning: t,
            tickets: bugs.iter().map(|id| open_bug(id)).collect(),
            ..ProjectState::default()
        }
    }

    #[test]
    fn burn_mode_exits_once_the_open_bug_count_reaches_its_target() {
        // The gate is the COUNT: even HIGH-priority open bugs at or below the
        // target release feature work.
        let mut s = state_with(tuning(true, Some(2), false), &["B001", "B002"]);
        assert!(!burn_mode_holds(&s), "count == target: the gate is met");
        assert!(clear_burn_mode_if_gate_met(&mut s), "a met gate clears");
        assert!(!s.tuning.burn_mode, "exit means the mode is off");
        assert!(!clear_burn_mode_if_gate_met(&mut s), "idempotent");
    }

    #[test]
    fn burn_mode_holds_features_while_open_bugs_exceed_the_target() {
        let mut s = state_with(tuning(true, Some(1), false), &["B001", "B002", "B003"]);
        assert!(burn_mode_holds(&s));
        assert!(
            !clear_burn_mode_if_gate_met(&mut s),
            "an unmet gate never clears the mode"
        );
        assert!(s.tuning.burn_mode);
    }

    #[test]
    fn burn_mode_without_a_numeric_gate_holds_until_a_person_disables_it() {
        let mut s = state_with(tuning(true, None, false), &[]);
        assert!(burn_mode_holds(&s));
        assert!(!clear_burn_mode_if_gate_met(&mut s));
    }

    #[test]
    fn reactive_bugs_first_pauses_independently_of_the_explicit_mode() {
        // The reactive brake alone holds features…
        let s = state_with(tuning(false, None, true), &[]);
        assert!(dev_feature_paused(&s, true));
        // …and the explicit mode is inert while switched off.
        assert!(!burn_mode_holds(&s));
        // A burn-mode exit never touches the reactive brake: clearing only
        // rewrites `burn_mode`.
        let mut both = state_with(tuning(true, Some(0), true), &[]);
        assert!(clear_burn_mode_if_gate_met(&mut both));
        assert!(both.tuning.bugs_first, "reactive brake survives the exit");
    }

    #[test]
    fn the_two_brakes_hold_features_with_or_semantics() {
        let burn_only = state_with(tuning(true, Some(1), false), &["B001", "B002"]);
        assert!(dev_feature_paused(&burn_only, true));
        let reactive_only = state_with(tuning(false, None, true), &[]);
        assert!(dev_feature_paused(&reactive_only, true));
        let neither = state_with(tuning(false, None, false), &[]);
        assert!(!dev_feature_paused(&neither, true));
        // The shared deadlock valve: a cycle whose bug slot produced nothing
        // never holds features, whatever the brakes say.
        let both = state_with(tuning(true, Some(0), true), &["B001"]);
        assert!(!dev_feature_paused(&both, false));
        assert!(dev_feature_paused(&both, true));
    }

    #[test]
    fn burn_down_scope_covers_blockers_and_bugs_preceding_the_next_feature() {
        let mut blocked = ready_feature("F002", Priority::High);
        blocked
            .add_dependency(Role::Sa, TicketId::new("B001").expect("id"))
            .expect("dep");
        let state = ProjectState {
            // B001 blocks F002 (its dep); B002 precedes it in the backlog;
            // B003 sits after the feature and blocks nothing — out of scope.
            tickets: vec![
                open_bug("B001"),
                open_bug("B002"),
                blocked,
                open_bug("B003"),
            ],
            ..ProjectState::default()
        };
        let scope = burn_down_scope(&state);
        assert!(scope.contains(&TicketId::new("B001").expect("id")));
        assert!(scope.contains(&TicketId::new("B002").expect("id")));
        assert!(!scope.contains(&TicketId::new("B003").expect("id")));
        // …and with no actionable feature work there is no scope at all.
        let empty = ProjectState {
            tickets: vec![open_bug("B001")],
            ..ProjectState::default()
        };
        assert!(burn_down_scope(&empty).is_empty());
    }

    #[test]
    fn a_blocking_bug_behind_the_feature_is_still_in_scope() {
        let mut feature = ready_feature("F001", Priority::High);
        feature
            .add_dependency(Role::Sa, TicketId::new("B009").expect("id"))
            .expect("dep");
        let state = ProjectState {
            // B009 does not precede F001, but the feature depends on it.
            tickets: vec![feature, open_bug("B009")],
            ..ProjectState::default()
        };
        let scope = burn_down_scope(&state);
        assert!(
            scope.contains(&TicketId::new("B009").expect("id")),
            "a dependency blocks the next feature work wherever it sits"
        );
    }
}
