//! The pure bookkeeping a sprint's scope is sized, filled and judged by:
//! `sprint_capacity` sizes the commitment from delivery history, `open_backlog`
//! picks what the backlog commits to, and `done_count` counts how much of that
//! commitment actually shipped. Pure over a `ProjectState` snapshot — no IO,
//! no writes; the sprint boundary writes live in `crate::sprint`, which reads
//! these to decide. Split out of `sprint.rs` (CXA-C015) so the commit-vs-done
//! mismatch is isolated bookkeeping, not one more block in the lifecycle module.

use crate::selection::{below_bug_burn_floor, burn_down_scope};
use crate::state::ProjectState;
use coxagent_domain::{Priority, Status, TicketId, TicketType};

/// Unshipped work a sprint commits to. Bugs count too — the sprint board used
/// to track only features/chores, so a team heads-down on a bug burndown
/// looked idle ("sprint không work gì hết") while two DEVs were mid-fix.
/// Open bugs commit first (they outrank new work), then features/chores.
/// `bug_burn_floor` (CXA-F028) scopes the commitment to bugs at or above that
/// priority: the high-severity burn parks cosmetics instead of committing them.
pub(crate) fn open_backlog(
    state: &ProjectState,
    bug_burn_floor: Option<Priority>,
) -> Vec<TicketId> {
    let cap = sprint_capacity(state);
    // A bug-heavy backlog must not lock FEATURE work out of every sprint:
    // reserve one slot for a READY feature/chore so DEV-FEATURE always has
    // something scoped to build. Without this, any sprint where open bugs do
    // not fit within capacity commits only bugs and ready features starve
    // under the sprint-scope gate — a full Ready queue, yet an idle dev.
    let mut picked: Vec<TicketId> = Vec::new();
    if let Some(ready) = state
        .tickets
        .iter()
        .find(|t| {
            matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
                && t.status() == Status::Ready
        })
        .map(|t| t.id().clone())
    {
        picked.push(ready);
    }
    // Burn-down scope (CXA-F030): every open bug that blocks or precedes the
    // next feature work IS the sprint's job — committed in full, never
    // capacity-capped. Capping it is how a burn-down sprint ships with its
    // own blockers still uncommitted. Disjoint from the seat above (scope is
    // bugs, the seat is a feature/chore), so nothing dedupes here.
    picked.extend(burn_down_scope(state));
    // Open bugs get the next seats, but only within remaining capacity — the
    // reserved feature slot above is never displaced by bug pressure. Bugs
    // below the burn floor are skipped: parked, not committed.
    let mut bug_budget = cap.saturating_sub(picked.len());
    for t in state.tickets.iter().filter(|t| {
        t.ticket_type() == TicketType::Bug
            && t.status() == Status::Open
            && !below_bug_burn_floor(bug_burn_floor, t.priority())
    }) {
        if bug_budget == 0 {
            break;
        }
        if picked.contains(t.id()) {
            continue;
        }
        picked.push(t.id().clone());
        bug_budget -= 1;
    }
    // Remaining capacity: the rest of the actionable features/chores.
    let seats = cap.saturating_sub(picked.len());
    let already_picked: std::collections::BTreeSet<TicketId> = picked.iter().cloned().collect();
    picked.extend(
        state
            .tickets
            .iter()
            .filter(|t| {
                matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
                    && !matches!(
                        t.status(),
                        Status::Done | Status::Documented | Status::Rejected
                    )
                    && !already_picked.contains(t.id())
            })
            .take(seats)
            .map(|t| t.id().clone()),
    );
    picked
}

/// How much to commit to one sprint: what the team has actually been finishing,
/// with a little stretch, never less than a few.
///
/// Committing the WHOLE backlog made every sprint a lie — 59 tickets in, ~0
/// out, every retro reporting 0% velocity and carrying all 59 forward. A sprint
/// that contains everything says nothing about what the team intends to do
/// next, and a goal derived from it is noise.
fn sprint_capacity(state: &ProjectState) -> usize {
    const FLOOR: usize = 3;
    let history: Vec<usize> = state.sprints.iter().rev().take(5).map(|s| s.done).collect();
    if history.is_empty() {
        return FLOOR * 2;
    }
    let avg = history.iter().sum::<usize>() / history.len().max(1);
    // Approved reverted work (CXA-F047) is value the velocity history credits
    // but the product never kept: each human-confirmed revert hands back one
    // seat, so the next cycle plans against delivery that actually stuck.
    // DELIBERATE timescale: the discount is durable (per-ticket learning),
    // unlike the last-5-sprint velocity window — a confirmed revert stays a
    // fact about that ticket. Only APPROVED events weigh in — a pending
    // detection may be a false positive and a dismissed one was judged to
    // be. The ledger is bounded (oldest trimmed) and clamped by FLOOR below,
    // so the discount can never zero out planning.
    let reverted = state
        .reverted_work
        .iter()
        .filter(|e| e.decision == crate::state::RevertDecision::Approved)
        .count();
    (avg + avg / 2).saturating_sub(reverted).max(FLOOR)
}

/// How many committed tickets have shipped — for the burndown/progress view.
#[must_use]
pub fn done_count(state: &ProjectState) -> usize {
    let Some(sprint) = &state.sprint else {
        return 0;
    };
    sprint
        .committed
        .iter()
        .filter(|id| {
            state
                .ticket(id)
                .is_some_and(|t| matches!(t.status(), Status::Done | Status::Documented))
        })
        .count()
}

/// The ticket builders shared by both halves of the sprint split: these serve
/// the bookkeeping tests here AND the lifecycle tests in `crate::sprint`
/// (sibling test modules cannot reach each other's `super::` privates, so the
/// shared scaffolding lives with the module that owns the scope rules).
#[cfg(test)]
pub(crate) mod fixtures {
    use coxagent_domain::{
        Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
    };

    pub(crate) fn feature(id: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t")
    }

    pub(crate) fn bug(id: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Bug,
            "b",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t")
    }

    pub(crate) fn bug_prio(id: &str, prio: Priority) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Bug,
            "b",
            "",
            prio,
            Complexity::Small,
            false,
        )
        .expect("t")
    }

    pub(crate) fn ready_feature(id: &str) -> Ticket {
        let mut t = feature(id);
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("design");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{bug, bug_prio, ready_feature};
    use super::{done_count, open_backlog, sprint_capacity};
    use crate::sprint::commit_ticket;
    use crate::state::{
        now_rfc3339, ProjectState, RevertDecision, RevertEvent, Sprint, SprintRecord,
    };
    use coxagent_domain::{Priority, Role, Status, TicketId};

    // ---- CXA-F047: approved reverted work discounts next-cycle planning. ----

    fn revert_event(ticket: &str, decision: RevertDecision) -> RevertEvent {
        let at = now_rfc3339();
        RevertEvent {
            sha: format!("sha-{ticket}"),
            subject: format!("Revert \"feat({ticket}): w\""),
            ticket: ticket.to_owned(),
            role: "DEV-FEATURE".to_owned(),
            reverted_at: at.clone(),
            detected_at: at,
            decision,
            decided_at: None,
            decided_by: None,
        }
    }

    fn velocity_state(dones: &[usize], reverts: &[RevertEvent]) -> ProjectState {
        ProjectState {
            sprints: dones
                .iter()
                .enumerate()
                .map(|(i, done)| SprintRecord {
                    number: u32::try_from(i + 1).unwrap_or(1),
                    goal: "g".to_owned(),
                    committed: *done,
                    done: *done,
                    at: String::new(),
                })
                .collect(),
            reverted_work: reverts.to_vec(),
            ..ProjectState::default()
        }
    }

    fn with_history(done: &[usize]) -> ProjectState {
        let mut s = ProjectState::default();
        for (i, d) in done.iter().enumerate() {
            s.sprints.push(SprintRecord {
                number: u32::try_from(i).unwrap_or(0) + 1,
                goal: String::new(),
                committed: 50,
                done: *d,
                at: String::new(),
            });
        }
        s
    }

    #[test]
    fn approved_reverts_discount_next_cycle_planning() {
        // Velocity 6 → capacity 9 (half-step of stretch).
        let clean = velocity_state(&[6, 6], &[]);
        assert_eq!(sprint_capacity(&clean), 9);
        // Only APPROVED events weigh in: a pending detection may be a false
        // positive and a dismissed one was judged to be.
        let pending = velocity_state(
            &[6, 6],
            &[revert_event("CXA-F041", RevertDecision::Pending)],
        );
        assert_eq!(sprint_capacity(&pending), 9);
        let dismissed = velocity_state(
            &[6, 6],
            &[revert_event("CXA-F041", RevertDecision::Dismissed)],
        );
        assert_eq!(sprint_capacity(&dismissed), 9);
        let approved = velocity_state(
            &[6, 6],
            &[
                revert_event("CXA-F041", RevertDecision::Approved),
                revert_event("CXA-B002", RevertDecision::Approved),
            ],
        );
        assert_eq!(sprint_capacity(&approved), 7);
    }

    #[test]
    fn the_revert_discount_never_breaks_the_capacity_floor() {
        // Even a revert per sprint keeps the sprint meaningful (FLOOR).
        let reverts: Vec<_> = (0..12)
            .map(|i| revert_event(&format!("CXA-F0{i:02}"), RevertDecision::Approved))
            .collect();
        let state = velocity_state(&[6, 6], &reverts);
        assert_eq!(sprint_capacity(&state), 3);
    }

    #[test]
    fn capacity_follows_what_the_team_actually_finished() {
        // A team shipping ~4 a sprint commits to 6, not to the whole backlog.
        assert_eq!(sprint_capacity(&with_history(&[4, 4, 4])), 6);
        // A brand-new project has no history to go on; start modest.
        assert_eq!(sprint_capacity(&with_history(&[])), 6);
        // Even a team that shipped nothing commits to something — a sprint of
        // zero would never recover.
        assert_eq!(sprint_capacity(&with_history(&[0, 0])), 3);
        // Only the recent past counts: an old heroic sprint does not license
        // over-committing forever.
        let long = with_history(&[40, 1, 1, 1, 1, 1]);
        assert_eq!(sprint_capacity(&long), 3);
    }

    // ---- open_backlog: what a fresh sprint commits to. ----

    #[test]
    fn a_bug_heavy_backlog_still_reserves_a_ready_feature_slot() {
        // A fresh state commits to 6 (FLOOR*2). A bug-heavy backlog used to
        // fill every seat with open bugs, locking a ready feature out of the
        // sprint entirely — an idle DEV-FEATURE with a full Ready queue. The
        // reserved feature slot prevents that.
        let state = ProjectState {
            tickets: vec![ready_feature("F001"), bug("B001"), bug("B002"), bug("B003")],
            ..ProjectState::default()
        };
        let committed = open_backlog(&state, None);
        assert!(
            committed.contains(&TicketId::new("F001").expect("id")),
            "a ready feature must keep a scout seat even under bug pressure"
        );
        // The reserved feature is NOT displaced: it sits at the front.
        assert_eq!(committed[0], TicketId::new("F001").expect("id"));
    }

    #[test]
    fn no_ready_feature_means_bugs_take_the_whole_backlog() {
        let state = ProjectState {
            tickets: vec![bug("B001"), bug("B002"), bug("B003"), bug("B004")],
            ..ProjectState::default()
        };
        let committed = open_backlog(&state, None);
        assert!(
            committed.iter().all(|id| id.to_string().starts_with('B')),
            "without a ready feature, the sprint is bugs-only"
        );
    }

    // ---- CXA-F028: the bug-burn floor scopes commitment and DEV work. ----

    #[test]
    fn a_burn_floor_scopes_commitment_to_at_or_above_floor_bugs() {
        let state = ProjectState {
            tickets: vec![
                ready_feature("F001"),
                bug_prio("B-HIGH", Priority::High),
                bug_prio("B-MED", Priority::Medium),
                bug_prio("B-LOW", Priority::Low),
            ],
            ..ProjectState::default()
        };
        let committed = open_backlog(&state, Some(Priority::High));
        assert!(
            committed.contains(&TicketId::new("B-HIGH").expect("id")),
            "high-severity bugs are the burn's target"
        );
        assert!(
            !committed.contains(&TicketId::new("B-MED").expect("id")),
            "medium bugs are parked below a High floor"
        );
        assert!(
            !committed.contains(&TicketId::new("B-LOW").expect("id")),
            "low bugs are parked below a High floor"
        );
        // The reserved feature slot survives the floor.
        assert!(committed.contains(&TicketId::new("F001").expect("id")));
    }

    #[test]
    fn no_floor_commits_every_open_bug_exactly_as_before() {
        let state = ProjectState {
            tickets: vec![
                bug_prio("B-HIGH", Priority::High),
                bug_prio("B-MED", Priority::Medium),
                bug_prio("B-LOW", Priority::Low),
            ],
            ..ProjectState::default()
        };
        assert_eq!(
            open_backlog(&state, None).len(),
            3,
            "floor=None burns every open bug (the historical behaviour)"
        );
    }

    // ---- CXA-C015: the commit→done mismatch, testable in isolation. ----

    fn running_sprint_with(committed: Vec<TicketId>) -> Sprint {
        Sprint {
            number: 1,
            goal: "g".into(),
            started_cycle: 1,
            length_cycles: 10,
            committed,
            started_at: now_rfc3339(),
            bug_burn_floor: None,
        }
    }

    #[test]
    fn a_committed_ticket_counts_only_once_it_reaches_done() {
        let mut state = ProjectState {
            tickets: vec![ready_feature("F001")],
            ..ProjectState::default()
        };
        state.sprint = Some(running_sprint_with(Vec::new()));
        // A commit is a promise, not a delivery: the whole point of this
        // accounting is that committed ≠ done until the work ships.
        assert!(commit_ticket(
            &mut state,
            &TicketId::new("F001").expect("id")
        ));
        assert_eq!(done_count(&state), 0);
        {
            let f1 = state
                .tickets
                .iter_mut()
                .find(|t| t.id().to_string() == "F001")
                .expect("t");
            f1.transition_to(Role::System, Status::InProgress)
                .expect("claim");
            f1.transition_to(Role::System, Status::Done).expect("done");
        }
        assert_eq!(done_count(&state), 1);
        {
            let f1 = state
                .tickets
                .iter_mut()
                .find(|t| t.id().to_string() == "F001")
                .expect("t");
            f1.transition_to(Role::System, Status::Documented)
                .expect("documented");
        }
        // Documented is still shipped truth (Done -> Documented), not a loss.
        assert_eq!(done_count(&state), 1);
    }

    #[test]
    fn done_count_judges_only_the_committed_scope() {
        // A ticket that shipped OUTSIDE the sprint's commitment is not sprint
        // delivery (it never was this sprint's job), and a committed id whose
        // ticket vanished from state is not phantom delivery either.
        let mut state = ProjectState {
            tickets: vec![ready_feature("F001")],
            ..ProjectState::default()
        };
        state.sprint = Some(running_sprint_with(
            vec![TicketId::new("F404").expect("id")],
        ));
        {
            let f1 = state
                .tickets
                .iter_mut()
                .find(|t| t.id().to_string() == "F001")
                .expect("t");
            f1.transition_to(Role::System, Status::InProgress)
                .expect("claim");
            f1.transition_to(Role::System, Status::Done).expect("done");
        }
        assert_eq!(done_count(&state), 0);
    }

    #[test]
    fn no_running_sprint_means_nothing_can_be_done() {
        // Kanban mode: with no sprint at all there is no committed scope to
        // judge — the guard the burndown view hits before any counting.
        assert_eq!(done_count(&ProjectState::default()), 0);
    }
}
