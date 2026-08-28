//! Sprint layer for scrum mode. Pure over state — the cycle calls `advance`
//! at the start of each cycle; it opens the first sprint and rolls over to a
//! new one when the window elapses, committing the open feature backlog.

use crate::state::{ProjectState, Sprint};
use coxagent_domain::{Status, TicketId, TicketType};

/// Open the first sprint or roll over an elapsed one. Returns the number of a
/// newly opened sprint, or `None` when the current sprint is still running.
/// How a sprint window elapses — the config's `sprint_unit`/lengths, resolved.
/// `Days` rolls on wall clock (what most teams mean by "a sprint" — cycles
/// shrank from ~30 min to ~90 s as the loop got faster, and counting only
/// cycles produced 500 seven-minute "sprints" in two days). `Cycles` keeps the
/// pure cycle counter for cadence experiments.
#[derive(Debug, Clone, Copy)]
pub enum SprintPolicy {
    Days(u64),
    Cycles(u64),
}

impl SprintPolicy {
    /// Resolve from the workflow config.
    #[must_use]
    pub fn from_config(wf: &crate::config::WorkflowConfig) -> Self {
        match wf.sprint_unit {
            crate::config::SprintUnit::Days => SprintPolicy::Days(wf.sprint_length_days.max(1)),
            crate::config::SprintUnit::Cycles => {
                SprintPolicy::Cycles(wf.sprint_length_cycles.max(1))
            }
        }
    }
}

pub fn advance(state: &mut ProjectState, cycle: u64, policy: SprintPolicy) -> Option<u32> {
    let need_open = match &state.sprint {
        None => true,
        Some(s) => match policy {
            // Cycle windows ALSO require a minimum wall-clock age (the promise
            // Sprint.started_at documents): cycles shrank from ~30 min to ~2
            // min as the loop got faster, and a pure cycle counter rolled a
            // "sprint" every 10 minutes — the team spent the whole day in
            // planning/review/retro ceremonies and DEV never shipped anything
            // (sprint 586's velocity-0% retro, tickets carried over forever).
            SprintPolicy::Cycles(len) => {
                cycle.saturating_sub(s.started_cycle) >= len
                    && sprint_age_minutes(&s.started_at) >= MIN_SPRINT_MINUTES
            }
            SprintPolicy::Days(days) => sprint_age_days(&s.started_at) >= days,
        },
    };
    if !need_open {
        return None;
    }
    let length = match policy {
        SprintPolicy::Cycles(len) => len,
        // Recorded for display; day-based sprints don't use it to roll.
        SprintPolicy::Days(_) => state.sprint.as_ref().map_or(0, |s| s.length_cycles),
    };
    Some(roll_over(state, cycle, length))
}

/// The floor under a cycle-window sprint: however fast the cycles spin, a
/// sprint younger than this never rolls — ceremonies must stay rarer than work.
const MIN_SPRINT_MINUTES: u64 = 240;

/// Whole minutes since `started_at`; missing/unparseable reads as ancient.
fn sprint_age_minutes(started_at: &str) -> u64 {
    let fmt = &time::format_description::well_known::Rfc3339;
    match time::OffsetDateTime::parse(started_at, fmt) {
        Ok(t) => {
            let d = time::OffsetDateTime::now_utc() - t;
            u64::try_from(d.whole_minutes().max(0)).unwrap_or(0)
        }
        Err(_) => u64::MAX,
    }
}

/// Whole days since `started_at`. A missing/unparseable stamp reads as ancient,
/// so pre-existing sprints roll once and pick up a stamp from then on.
fn sprint_age_days(started_at: &str) -> u64 {
    let fmt = &time::format_description::well_known::Rfc3339;
    match time::OffsetDateTime::parse(started_at, fmt) {
        Ok(t) => {
            let d = time::OffsetDateTime::now_utc() - t;
            u64::try_from(d.whole_days().max(0)).unwrap_or(0)
        }
        Err(_) => u64::MAX,
    }
}

/// Archive whatever sprint is running and open the next one. The single place
/// a sprint boundary is written — the timed rollover and an early close must
/// leave identical history, or the velocity chart compares different shapes.
fn roll_over(state: &mut ProjectState, cycle: u64, length: u64) -> u32 {
    // Archive the closing sprint's outcome for the velocity history.
    if let Some(closing) = &state.sprint {
        let done = done_count(state);
        let record = crate::state::SprintRecord {
            number: closing.number,
            goal: closing.goal.clone(),
            committed: closing.committed.len(),
            done,
            at: crate::state::now_rfc3339(),
        };
        state.sprints.push(record);
    }
    let number = state.sprint.as_ref().map_or(0, |s| s.number) + 1;
    // A planned sprint at the queue front wins outright: its ticket set and
    // goal ARE the next sprint (that is what planning ahead means). Tickets
    // that shipped or vanished since planning are skipped; a plan whose every
    // ticket is gone falls back to the capacity-based auto-commit.
    let planned = if state.sprint_queue.is_empty() {
        None
    } else {
        Some(state.sprint_queue.remove(0))
    };
    let (committed, planned_goal) = match planned {
        Some(p) => {
            let live: Vec<TicketId> = p
                .tickets
                .iter()
                .filter(|id| {
                    state
                        .ticket(id)
                        .is_some_and(|t| {
                            !matches!(
                                t.status(),
                                Status::Documented
                                    | Status::Verified
                                    | Status::Rejected
                                    | Status::OnHold
                            )
                        })
                })
                .cloned()
                .collect();
            if live.is_empty() {
                (open_backlog(state), Some(p.goal))
            } else {
                (live, Some(p.goal))
            }
        }
        None => (open_backlog(state), None),
    };
    // Goal precedence: the plan's goal, then the PO's goal chip, then one
    // derived from the committed titles.
    let goal = match planned_goal.filter(|g| !g.trim().is_empty()) {
        Some(g) => g.trim().to_owned(),
        None if !state.sprint_goal.trim().is_empty() => state.sprint_goal.trim().to_owned(),
        None => goal_from(state, &committed),
    };
    state.sprint = Some(Sprint {
        number,
        goal,
        started_cycle: cycle,
        length_cycles: length,
        committed,
        started_at: crate::state::now_rfc3339(),
    });
    number
}

/// Pull a ticket into the sprint that is already running.
///
/// The automatic commit is capacity-based and happens once, at rollover; a
/// person who decides mid-sprint that something belongs in it had no way to
/// say so. Returns whether the sprint changed — a ticket already committed, a
/// ticket that does not exist, or no open sprint all mean "nothing to do".
pub fn commit_ticket(state: &mut ProjectState, id: &TicketId) -> bool {
    if state.ticket(id).is_none() {
        return false;
    }
    let Some(sprint) = &mut state.sprint else {
        return false;
    };
    if sprint.committed.contains(id) {
        return false;
    }
    sprint.committed.push(id.clone());
    true
}

/// Refill a running sprint's scope when it would otherwise idle DEV.
///
/// Two cases, both silent starvation under the sprint-scope DEV gate:
///   1. The committed set is EMPTY — even as tickets become Ready mid-sprint,
///      nothing is scoped until rollover (a full queue, an idle team).
///   2. The committed set is bug-only while READY features sit outside it — a
///      bug-heavy sprint that, without intervention, never gives DEV-FEATURE
///      scoped work (the same lock-out `open_backlog`'s reserved feature slot
///      prevents at rollover, healed here mid-sprint for an already-open one).
///
/// Returns how many tickets were committed (0 = nothing needed).
pub fn refill_empty_scope(state: &mut ProjectState) -> usize {
    // Case 1: a genuinely empty committed set — pull the whole backlog in.
    if state
        .sprint
        .as_ref()
        .is_some_and(|s| s.committed.is_empty())
    {
        let backlog = open_backlog(state);
        let n = backlog.len();
        if let Some(sprint) = &mut state.sprint {
            sprint.committed = backlog;
        }
        return n;
    }
    // Case 2: committed but feature-less — reserve one ready feature (the
    // live starvation: sprint #561 was bug-only while 44 features sat Ready).
    let has_feature = state.sprint.as_ref().is_some_and(|s| {
        s.committed.iter().any(|id| {
            state
                .ticket(id)
                .is_some_and(|t| matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore))
        })
    });
    if has_feature {
        return 0;
    }
    let committed: Vec<&TicketId> = state
        .sprint
        .as_ref()
        .map(|s| s.committed.iter().collect())
        .unwrap_or_default();
    if let Some(ready) = state.tickets.iter().find(|t| {
        matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
            && t.status() == Status::Ready
            && !committed.contains(&t.id())
    }) {
        if let Some(sprint) = &mut state.sprint {
            sprint.committed.push(ready.id().clone());
            return 1;
        }
    }
    0
}

/// Drop a ticket from the running sprint — scope a sprint DOWN mid-flight
/// without deleting the ticket.
pub fn uncommit_ticket(state: &mut ProjectState, id: &TicketId) -> bool {
    let Some(sprint) = &mut state.sprint else {
        return false;
    };
    let before = sprint.committed.len();
    sprint.committed.retain(|c| c != id);
    sprint.committed.len() != before
}

/// Close the running sprint NOW and open the next one, instead of waiting for
/// the window to elapse. The archived record keeps the velocity history
/// honest: what was committed and what actually shipped, same as a rollover.
///
/// Returns the new sprint number, or `None` when no sprint was running.
pub fn close_now(state: &mut ProjectState, cycle: u64) -> Option<u32> {
    let length = state.sprint.as_ref()?.length_cycles.max(1);
    Some(roll_over(state, cycle, length))
}

/// A readable goal from the first few committed ticket titles.
fn goal_from(state: &ProjectState, committed: &[TicketId]) -> String {
    let titles: Vec<&str> = committed
        .iter()
        .filter_map(|id| state.ticket(id).map(coxagent_domain::Ticket::title))
        .take(3)
        .collect();
    if titles.is_empty() {
        "Stabilise and polish".to_owned()
    } else {
        format!("Ship {}", titles.join(", "))
    }
}

/// Unshipped work a sprint commits to. Bugs count too — the sprint board used
/// to track only features/chores, so a team heads-down on a bug burndown
/// looked idle ("sprint không work gì hết") while two DEVs were mid-fix.
/// Open bugs commit first (they outrank new work), then features/chores.
fn open_backlog(state: &ProjectState) -> Vec<TicketId> {
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
    // Open bugs get the next seats, but only within remaining capacity — the
    // reserved feature slot above is never displaced by bug pressure.
    let mut bug_budget = cap.saturating_sub(picked.len());
    for t in state
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
    {
        if bug_budget == 0 {
            break;
        }
        picked.push(t.id().clone());
        bug_budget -= 1;
    }
    // Remaining capacity: the rest of the actionable features/chores.
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
            })
            .take(cap.saturating_sub(picked.len()))
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
    // A half-step of stretch over the measured average — enough to pull ahead
    // on a good sprint, not enough to make the number meaningless again.
    (avg + avg / 2).max(FLOOR)
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


/// Queue a sprint to run after the current one. Returns the new plan's id.
pub fn queue_sprint(state: &mut ProjectState, goal: &str, tickets: Vec<TicketId>, by: &str) -> u64 {
    let id = state
        .sprint_queue
        .iter()
        .map(|p| p.id)
        .max()
        .unwrap_or(0)
        + 1;
    // Only tickets that exist can be planned; duplicates collapse.
    let mut seen = std::collections::BTreeSet::new();
    let tickets: Vec<TicketId> = tickets
        .into_iter()
        .filter(|t| state.ticket(t).is_some() && seen.insert(t.clone()))
        .collect();
    state.sprint_queue.push(crate::state::PlannedSprint {
        id,
        goal: goal.trim().to_owned(),
        tickets,
        created_at: crate::state::now_rfc3339(),
        by: by.to_owned(),
    });
    id
}

/// Add or remove tickets on a queued sprint. Unknown plan id → `false`.
pub fn scope_queued_sprint(
    state: &mut ProjectState,
    id: u64,
    add: &[TicketId],
    remove: &[TicketId],
) -> bool {
    // Existence is checked against tickets before the mutable borrow.
    let valid: Vec<TicketId> = add
        .iter()
        .filter(|t| state.ticket(t).is_some())
        .cloned()
        .collect();
    let Some(plan) = state.sprint_queue.iter_mut().find(|p| p.id == id) else {
        return false;
    };
    plan.tickets.retain(|t| !remove.contains(t));
    for t in valid {
        if !plan.tickets.contains(&t) {
            plan.tickets.push(t);
        }
    }
    true
}

/// Drop a queued sprint outright. Unknown plan id → `false`.
pub fn delete_queued_sprint(state: &mut ProjectState, id: u64) -> bool {
    let before = state.sprint_queue.len();
    state.sprint_queue.retain(|p| p.id != id);
    state.sprint_queue.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Complexity, Priority, Role, TechnicalDesign, Ticket, TicketType};

    fn feature(id: &str) -> Ticket {
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

    fn bug(id: &str) -> Ticket {
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

    fn ready_feature(id: &str) -> Ticket {
        let mut t = feature(id);
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("design");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t
    }

    #[test]
    fn rollover_consumes_the_planned_queue_front_first() {
        let mut state = ProjectState {
            tickets: vec![feature("F001"), feature("F002"), feature("F003")],
            ..ProjectState::default()
        };
        let id1 = queue_sprint(
            &mut state,
            "harden auth",
            vec![TicketId::new("F002").expect("id")],
            "po",
        );
        queue_sprint(&mut state, "polish UI", vec![], "po");
        assert!(id1 > 0);
        advance(&mut state, 1, SprintPolicy::Cycles(10));
        let sp = state.sprint.as_ref().expect("sprint");
        assert_eq!(sp.goal, "harden auth");
        assert_eq!(sp.committed, vec![TicketId::new("F002").expect("id")]);
        // The consumed plan is gone; the next one waits its turn.
        assert_eq!(state.sprint_queue.len(), 1);
        assert_eq!(state.sprint_queue[0].goal, "polish UI");
    }

    #[test]
    fn an_empty_queue_leaves_rollover_exactly_as_before() {
        let mut a = ProjectState {
            tickets: vec![feature("F001"), feature("F002")],
            ..ProjectState::default()
        };
        let mut b = a.clone();
        advance(&mut a, 1, SprintPolicy::Cycles(10));
        advance(&mut b, 1, SprintPolicy::Cycles(10));
        let (sa, sb) = (a.sprint.expect("a"), b.sprint.expect("b"));
        // started_at is a wall-clock stamp; everything meaningful must match.
        assert_eq!((sa.number, &sa.goal, &sa.committed), (sb.number, &sb.goal, &sb.committed));
    }

    #[test]
    fn a_plan_whose_tickets_all_shipped_falls_back_to_auto_commit() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        // Plan names a ticket that no longer exists by rollover time.
        state.sprint_queue.push(crate::state::PlannedSprint {
            id: 1,
            goal: "ghost plan".to_owned(),
            tickets: vec![TicketId::new("F999").expect("id")],
            created_at: String::new(),
            by: "po".to_owned(),
        });
        advance(&mut state, 1, SprintPolicy::Cycles(10));
        let sp = state.sprint.as_ref().expect("sprint");
        assert_eq!(sp.goal, "ghost plan");
        // Fallback committed the real backlog instead of an empty scope.
        assert_eq!(sp.committed, vec![TicketId::new("F001").expect("id")]);
    }

    #[test]
    fn queued_sprint_scope_adds_dedupes_and_removes() {
        let mut state = ProjectState {
            tickets: vec![feature("F001"), feature("F002")],
            ..ProjectState::default()
        };
        let id = queue_sprint(&mut state, "g", vec![], "user");
        let f1 = TicketId::new("F001").expect("id");
        let f2 = TicketId::new("F002").expect("id");
        let ghost = TicketId::new("F999").expect("id");
        assert!(scope_queued_sprint(&mut state, id, &[f1.clone(), f1.clone(), ghost], &[]));
        assert_eq!(state.sprint_queue[0].tickets, vec![f1.clone()]);
        assert!(scope_queued_sprint(&mut state, id, &[f2.clone()], &[f1]));
        assert_eq!(state.sprint_queue[0].tickets, vec![f2]);
        assert!(!scope_queued_sprint(&mut state, 999, &[], &[]));
        assert!(delete_queued_sprint(&mut state, id));
        assert!(state.sprint_queue.is_empty());
    }

    #[test]
    fn opens_first_sprint_and_commits_backlog() {
        let mut state = ProjectState {
            tickets: vec![feature("F001"), feature("F002")],
            ..ProjectState::default()
        };
        let n = advance(&mut state, 1, SprintPolicy::Cycles(10));
        assert_eq!(n, Some(1));
        let s = state.sprint.as_ref().expect("sprint");
        assert_eq!(s.number, 1);
        assert_eq!(s.committed.len(), 2);
    }

    #[test]
    fn an_empty_sprint_scope_refills_from_backlog_instead_of_starving_dev() {
        // Sprint opened onto an empty backlog…
        let mut state = ProjectState::default();
        advance(&mut state, 1, SprintPolicy::Cycles(10));
        assert!(state.sprint.as_ref().expect("sprint").committed.is_empty());
        // …then tickets became Ready mid-sprint. Under the sprint-scope DEV
        // gate they'd be invisible until rollover — refill commits them now.
        state.tickets.push(feature("F001"));
        assert_eq!(refill_empty_scope(&mut state), 1);
        // Already-scoped sprints are never rewritten.
        assert_eq!(refill_empty_scope(&mut state), 0);
        // No sprint at all (Kanban): nothing to do.
        state.sprint = None;
        assert_eq!(refill_empty_scope(&mut state), 0);
    }

    #[test]
    fn a_bug_only_sprint_refills_a_ready_feature_mid_flight() {
        // Sprint #561 symptom: committed is non-empty (bugs) but feature-less,
        // while 44 features sit Ready — DEV-FEATURE silently starves. The old
        // refill only handled the EMPTY set; this heals the bug-only case too.
        let mut state = ProjectState {
            tickets: vec![bug("B001"), ready_feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::Cycles(10));
        // The rollover now reserves F001 (open_backlog change), so simulate an
        // OLD bug-only sprint that predates the fix.
        let f1 = TicketId::new("F001").expect("id");
        state
            .sprint
            .as_mut()
            .expect("sprint")
            .committed
            .retain(|c| c != &f1);
        assert!(
            refill_empty_scope(&mut state) >= 1,
            "a bug-only sprint must pull in a ready feature so DEV-FEATURE runs"
        );
        let s = state.sprint.as_ref().expect("sprint");
        assert!(
            s.committed.contains(&f1),
            "committed set must now hold the ready feature"
        );
        // Idempotent: a sprint that already has feature scope is left alone.
        assert_eq!(refill_empty_scope(&mut state), 0);
    }

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
        let committed = open_backlog(&state);
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
        let committed = open_backlog(&state);
        assert!(
            committed.iter().all(|id| id.to_string().starts_with('B')),
            "without a ready feature, the sprint is bugs-only"
        );
    }

    #[test]
    fn a_person_can_pull_a_ticket_into_the_running_sprint() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::Cycles(10));
        state.tickets.push(feature("F002"));
        let id = TicketId::new("F002").expect("id");
        assert!(commit_ticket(&mut state, &id));
        assert!(
            !commit_ticket(&mut state, &id),
            "committing twice must not duplicate the id"
        );
        assert!(state
            .sprint
            .as_ref()
            .expect("sprint")
            .committed
            .contains(&id));
        assert!(
            !commit_ticket(&mut state, &TicketId::new("F404").expect("id")),
            "a ticket that does not exist cannot be committed"
        );
        assert!(uncommit_ticket(&mut state, &id));
        assert!(!uncommit_ticket(&mut state, &id));
    }

    #[test]
    fn closing_early_archives_the_sprint_and_opens_the_next() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::Cycles(10));
        // Cycle 3 of a 10-cycle window: nothing would roll over on its own.
        assert_eq!(advance(&mut state, 3, SprintPolicy::Cycles(10)), None);
        assert_eq!(close_now(&mut state, 3), Some(2));
        assert_eq!(state.sprints.len(), 1, "the closed sprint is in history");
        assert_eq!(state.sprints[0].number, 1);
        assert_eq!(state.sprint.as_ref().expect("sprint").number, 2);
    }

    #[test]
    fn closing_with_no_sprint_running_is_a_no_op() {
        let mut state = ProjectState::default();
        assert_eq!(close_now(&mut state, 1), None);
        assert!(state.sprint.is_none());
    }

    #[test]
    fn does_not_reopen_mid_sprint() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::Cycles(10));
        assert_eq!(advance(&mut state, 5, SprintPolicy::Cycles(10)), None);
        assert_eq!(state.sprint.as_ref().expect("s").number, 1);
    }

    #[test]
    fn cycles_policy_needs_both_the_counter_and_real_time() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::Cycles(10));
        // Counter elapsed but the sprint is seconds old: must NOT roll. Pure
        // cycle-rolling at ~2-min cycles produced 10-minute "sprints" that
        // were all ceremony and no delivery (sprint 586, velocity 0%).
        assert_eq!(advance(&mut state, 11, SprintPolicy::Cycles(10)), None);
        // Same counter, but the sprint is genuinely old — now it rolls.
        if let Some(s) = &mut state.sprint {
            s.started_at = "2020-01-01T00:00:00Z".to_owned();
        }
        assert_eq!(advance(&mut state, 11, SprintPolicy::Cycles(10)), Some(2));
    }

    #[test]
    fn days_policy_ignores_cycle_count_until_the_day_passes() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::Days(1));
        // A thousand cycles later but seconds old: must NOT roll — this is the
        // 500-seven-minute-sprints bug the day unit exists to kill.
        assert_eq!(advance(&mut state, 1000, SprintPolicy::Days(1)), None);
        assert_eq!(state.sprint.as_ref().expect("s").number, 1);
        // Age it past a day: rolls.
        let old = (time::OffsetDateTime::now_utc() - time::Duration::hours(25))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("fmt");
        state.sprint.as_mut().expect("s").started_at = old;
        assert_eq!(advance(&mut state, 1000, SprintPolicy::Days(1)), Some(2));
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::sprint_capacity;
    use crate::state::{ProjectState, SprintRecord};

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
}
