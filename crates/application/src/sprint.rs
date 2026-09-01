//! Sprint layer for scrum mode. Pure over state — the cycle calls `advance`
//! at the start of each cycle; it opens the first sprint and rolls over to a
//! new one when the window elapses, committing the open feature backlog.

use crate::backlog_scoping::open_backlog;
use crate::selection::below_bug_burn_floor;
use crate::state::{ProjectState, Sprint};
use coxagent_domain::{Priority, Status, TicketId, TicketType};

/// Scope bookkeeping lives in its own unit since CXA-C015; re-exported so
/// every existing `sprint::done_count` call site (metrics, ceremonies, the
/// presentation server) keeps its path.
pub use crate::backlog_scoping::done_count;

/// Open the first sprint or roll over an elapsed one. Returns the number of a
/// newly opened sprint, or `None` when the current sprint is still running.
/// How a sprint window elapses — the config's `sprint_unit`/lengths, resolved.
/// `Days` rolls on wall clock (what most teams mean by "a sprint" — cycles
/// shrank from ~30 min to ~90 s as the loop got faster, and counting only
/// cycles produced 500 seven-minute "sprints" in two days). `Cycles` keeps the
/// pure cycle counter for cadence experiments.
#[derive(Debug, Clone, Copy)]
pub enum SprintWindow {
    Days(u64),
    Cycles(u64),
}

/// Everything a sprint boundary needs, resolved from the workflow config: how
/// the window elapses, plus the bug-burn floor mirrored onto the next Sprint
/// at rollover (CXA-F028). One vehicle on purpose — a caller that builds the
/// policy from config cannot forget to carry the floor with it.
#[derive(Debug, Clone, Copy)]
pub struct SprintPolicy {
    pub window: SprintWindow,
    pub bug_burn_floor: Option<Priority>,
}

impl SprintPolicy {
    /// Resolve from the workflow config.
    #[must_use]
    pub fn from_config(wf: &crate::config::WorkflowConfig) -> Self {
        let window = match wf.sprint_unit {
            crate::config::SprintUnit::Days => SprintWindow::Days(wf.sprint_length_days.max(1)),
            crate::config::SprintUnit::Cycles => {
                SprintWindow::Cycles(wf.sprint_length_cycles.max(1))
            }
        };
        Self {
            window,
            bug_burn_floor: wf.bug_burn_floor,
        }
    }

    /// Cycle-counted window, no burn floor.
    #[must_use]
    pub fn cycles(len: u64) -> Self {
        Self {
            window: SprintWindow::Cycles(len),
            bug_burn_floor: None,
        }
    }

    /// Day-counted window, no burn floor.
    #[must_use]
    pub fn days(days: u64) -> Self {
        Self {
            window: SprintWindow::Days(days),
            bug_burn_floor: None,
        }
    }
}

pub fn advance(state: &mut ProjectState, cycle: u64, policy: SprintPolicy) -> Option<u32> {
    let need_open = match &state.sprint {
        None => true,
        Some(s) => match policy.window {
            // Cycle windows ALSO require a minimum wall-clock age (the promise
            // Sprint.started_at documents): cycles shrank from ~30 min to ~2
            // min as the loop got faster, and a pure cycle counter rolled a
            // "sprint" every 10 minutes — the team spent the whole day in
            // planning/review/retro ceremonies and DEV never shipped anything
            // (sprint 586's velocity-0% retro, tickets carried over forever).
            SprintWindow::Cycles(len) => {
                cycle.saturating_sub(s.started_cycle) >= len
                    && sprint_age_minutes(&s.started_at) >= MIN_SPRINT_MINUTES
            }
            SprintWindow::Days(days) => sprint_age_days(&s.started_at) >= days,
        },
    };
    if !need_open {
        return None;
    }
    let length = match policy.window {
        SprintWindow::Cycles(len) => len,
        // Recorded for display; day-based sprints don't use it to roll.
        SprintWindow::Days(_) => state.sprint.as_ref().map_or(0, |s| s.length_cycles),
    };
    Some(roll_over(state, cycle, length, policy.bug_burn_floor))
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
/// `bug_burn_floor` is the config's floor mirrored onto the new sprint
/// (CXA-F028), so selection reads a pure fact of state from here on.
fn roll_over(
    state: &mut ProjectState,
    cycle: u64,
    length: u64,
    bug_burn_floor: Option<Priority>,
) -> u32 {
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
                    state.ticket(id).is_some_and(|t| {
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
                (open_backlog(state, bug_burn_floor), Some(p.goal))
            } else {
                (live, Some(p.goal))
            }
        }
        None => (open_backlog(state, bug_burn_floor), None),
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
        bug_burn_floor,
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
    // Case 1: a genuinely empty committed set — pull the backlog in, still
    // respecting the running sprint's burn floor (an active burn does not
    // re-admit parked cosmetics through the refill door).
    if state
        .sprint
        .as_ref()
        .is_some_and(|s| s.committed.is_empty())
    {
        let floor = state.sprint.as_ref().and_then(|s| s.bug_burn_floor);
        let backlog = open_backlog(state, floor);
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
    // Keep the closing sprint's burn floor: an early close re-opens the next
    // sprint under the same burn (only the config changes the floor).
    let s = state.sprint.as_ref()?;
    let (length, bug_burn_floor) = (s.length_cycles.max(1), s.bug_burn_floor);
    Some(roll_over(state, cycle, length, bug_burn_floor))
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

/// Queue a sprint to run after the current one. Returns the new plan's id.
pub fn queue_sprint(state: &mut ProjectState, goal: &str, tickets: Vec<TicketId>, by: &str) -> u64 {
    let id = state.sprint_queue.iter().map(|p| p.id).max().unwrap_or(0) + 1;
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

/// The fail-attempt count at which a ticket stops being retried and is parked.
pub const HOLD_AFTER_ATTEMPTS: u32 = 3;

/// Auto-park exhausted tickets: anything that failed `HOLD_AFTER_ATTEMPTS`
/// times and still sits in play (Pending/Ready/Open) moves to `OnHold` and
/// leaves the running sprint's scope. Before this, "parked" was only a KPI:
/// the ticket kept re-entering every sprint (the CXA-B072/B073 billing bugs
/// recommitted themselves for days and starved DEV). A person resumes it from
/// the ticket dialog when the outside blocker is gone — resume clears the
/// attempt count, so the retry starts fresh.
pub fn auto_hold_exhausted(state: &mut ProjectState) -> Vec<TicketId> {
    let exhausted: Vec<TicketId> = state
        .tickets
        .iter()
        .filter(|t| {
            matches!(t.status(), Status::Pending | Status::Ready | Status::Open)
                && state
                    .ticket_fail_attempts
                    .get(&t.id().to_string())
                    .is_some_and(|n| *n >= HOLD_AFTER_ATTEMPTS)
        })
        .map(|t| t.id().clone())
        .collect();
    let mut held = Vec::new();
    for id in exhausted {
        let ok = state
            .tickets
            .iter_mut()
            .find(|t| t.id() == &id)
            .is_some_and(|t| {
                t.transition_to(coxagent_domain::Role::System, Status::OnHold)
                    .is_ok()
            });
        if ok {
            if let Some(sp) = &mut state.sprint {
                sp.committed.retain(|c| c != &id);
            }
            state.hold_reasons.insert(
                id.to_string(),
                format!("auto-held after {HOLD_AFTER_ATTEMPTS} failed attempts"),
            );
            state.log_activity(
                "SYSTEM",
                "auto-held after repeated failures — resume it from the ticket when unblocked",
                Some(id.to_string()),
            );
            held.push(id);
        }
    }
    held
}

/// Forget a ticket's failure history — the other half of resume-from-hold:
/// without this, an auto-held ticket would be re-held on the next sweep.
pub fn clear_fail_attempts(state: &mut ProjectState, id: &TicketId) {
    state.ticket_fail_attempts.remove(&id.to_string());
    state.hold_reasons.remove(&id.to_string());
}

/// Rename a queued sprint's goal. Unknown id → `false`.
pub fn rename_queued_sprint(state: &mut ProjectState, id: u64, goal: &str) -> bool {
    let Some(p) = state.sprint_queue.iter_mut().find(|p| p.id == id) else {
        return false;
    };
    goal.trim().clone_into(&mut p.goal);
    true
}

/// Move a queued sprint one slot up (`-1`) or down (`+1`) in run order.
/// Unknown id or a move off either end → `false` (nothing changes).
pub fn move_queued_sprint(state: &mut ProjectState, id: u64, delta: i64) -> bool {
    let Some(i) = state.sprint_queue.iter().position(|p| p.id == id) else {
        return false;
    };
    let j = i64::try_from(i).unwrap_or(i64::MAX) + delta;
    if j < 0 || usize::try_from(j).unwrap_or(usize::MAX) >= state.sprint_queue.len() {
        return false;
    }
    state.sprint_queue.swap(i, usize::try_from(j).unwrap_or(i));
    true
}

/// The floor under a running sprint's actionable scope: when fewer than this
/// many committed tickets are still workable (Ready feature/chore or Open
/// bug), the top-up commits more from the backlog. Velocity-based capacity
/// sizes the sprint at rollover; this keeps DEV fed BETWEEN rollovers, so
/// finishing the scope early means more work, not an idle afternoon.
pub const MIN_ACTIONABLE_SCOPE: usize = 4; // default for config dev_scope_floor

/// Keep the running sprint's scope topped up to [`MIN_ACTIONABLE_SCOPE`].
/// Pulls Ready features/chores first (priority order is the backlog's own),
/// then Open bugs; anything OnHold/Rejected never qualifies. Returns how many
/// tickets were committed.
/// The goal text of the first milestone whose scope is not complete — the one
/// the roadmap timeline shows as "in progress".
fn active_milestone_goal(state: &ProjectState) -> Option<String> {
    state
        .milestones
        .iter()
        .find(|m| !m.goal_complete)
        .map(|m| format!("{} {}", m.name, m.goal).to_lowercase())
}

/// Crude, dependency-free relevance: the ticket title shares at least one
/// meaningful (5+ char) word with the milestone's name/goal text.
fn pushes_goal(goal: &str, title: &str) -> bool {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 5)
        .any(|w| goal.contains(w))
}

pub fn top_up_scope(state: &mut ProjectState, floor: usize) -> usize {
    if let Some(sprint) = &mut state.sprint {
        // Concurrent refill/top-up races have produced doubles; keep the
        // committed list a set whatever the interleaving was.
        let mut seen = std::collections::BTreeSet::new();
        sprint.committed.retain(|id| seen.insert(id.clone()));
    }
    let Some(sprint) = &state.sprint else {
        return 0;
    };
    // An active burn floor parks below-severity open bugs — the top-up must
    // not re-commit them through the back door (CXA-F028).
    let bug_burn_floor = sprint.bug_burn_floor;
    let committed: std::collections::BTreeSet<TicketId> =
        sprint.committed.iter().cloned().collect();
    let actionable = state
        .tickets
        .iter()
        .filter(|t| {
            committed.contains(t.id()) && matches!(t.status(), Status::Ready | Status::Open)
        })
        .count();
    if actionable >= floor {
        return 0;
    }
    let want = floor - actionable;
    // The PO's pick order, not list order: highest priority first, and inside
    // a priority band tickets that push the ACTIVE milestone (first one whose
    // goal is not complete) come before unrelated work — the roadmap moves
    // instead of only the newest backlog.
    let goal = active_milestone_goal(state);
    let mut ranked: Vec<(u8, u8, TicketId)> = state
        .tickets
        .iter()
        .filter(|t| {
            !committed.contains(t.id())
                && match t.status() {
                    Status::Ready => true,
                    Status::Open => !below_bug_burn_floor(bug_burn_floor, t.priority()),
                    _ => false,
                }
        })
        .map(|t| {
            let prio = match t.priority() {
                Priority::High => 0u8,
                Priority::Medium => 1,
                Priority::Low => 2,
            };
            let ms = u8::from(!goal.as_deref().is_some_and(|g| pushes_goal(g, t.title())));
            (prio, ms, t.id().clone())
        })
        .collect();
    ranked.sort();
    let picks: Vec<TicketId> = ranked.into_iter().map(|(_, _, id)| id).take(want).collect();
    let n = picks.len();
    if let Some(sprint) = &mut state.sprint {
        sprint.committed.extend(picks);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    // The ticket builders moved with the scope bookkeeping (CXA-C015); the
    // lifecycle tests reach across the sibling boundary for the same fixtures.
    use crate::backlog_scoping::fixtures::{bug, bug_prio, feature, ready_feature};

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
        advance(&mut state, 1, SprintPolicy::cycles(10));
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
        advance(&mut a, 1, SprintPolicy::cycles(10));
        advance(&mut b, 1, SprintPolicy::cycles(10));
        let (sa, sb) = (a.sprint.expect("a"), b.sprint.expect("b"));
        // started_at is a wall-clock stamp; everything meaningful must match.
        assert_eq!(
            (sa.number, &sa.goal, &sa.committed),
            (sb.number, &sb.goal, &sb.committed)
        );
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
        advance(&mut state, 1, SprintPolicy::cycles(10));
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
        assert!(scope_queued_sprint(
            &mut state,
            id,
            &[f1.clone(), f1.clone(), ghost],
            &[]
        ));
        assert_eq!(state.sprint_queue[0].tickets, vec![f1.clone()]);
        assert!(scope_queued_sprint(
            &mut state,
            id,
            std::slice::from_ref(&f2),
            &[f1]
        ));
        assert_eq!(state.sprint_queue[0].tickets, vec![f2]);
        assert!(!scope_queued_sprint(&mut state, 999, &[], &[]));
        assert!(delete_queued_sprint(&mut state, id));
        assert!(state.sprint_queue.is_empty());
    }

    #[test]
    fn exhausted_tickets_auto_hold_and_leave_sprint_scope() {
        let mut state = ProjectState {
            tickets: vec![feature("F001"), bug("B001")],
            ..ProjectState::default()
        };
        state.ticket_fail_attempts.insert("B001".into(), 3);
        state.ticket_fail_attempts.insert("F001".into(), 2);
        advance(&mut state, 1, SprintPolicy::cycles(10));
        assert!(state
            .sprint
            .as_ref()
            .expect("sprint")
            .committed
            .contains(&TicketId::new("B001").expect("id")));
        let held = auto_hold_exhausted(&mut state);
        assert_eq!(held, vec![TicketId::new("B001").expect("id")]);
        let b = state
            .ticket(&TicketId::new("B001").expect("id"))
            .expect("t");
        assert_eq!(b.status(), Status::OnHold);
        // Out of the running sprint; the 2-attempt ticket is untouched.
        assert!(!state
            .sprint
            .as_ref()
            .expect("sprint")
            .committed
            .contains(&TicketId::new("B001").expect("id")));
        // Resume + cleared attempts = eligible again, not instantly re-held.
        clear_fail_attempts(&mut state, &TicketId::new("B001").expect("id"));
        assert!(auto_hold_exhausted(&mut state).is_empty());
    }

    #[test]
    fn top_up_keeps_the_scope_floor_and_skips_held_tickets() {
        let mut state = ProjectState {
            tickets: vec![
                ready_feature("F001"),
                ready_feature("F002"),
                ready_feature("F003"),
                ready_feature("F004"),
                ready_feature("F005"),
            ],
            ..ProjectState::default()
        };
        state.sprint = Some(Sprint {
            number: 1,
            goal: "g".into(),
            started_cycle: 1,
            length_cycles: 10,
            committed: vec![TicketId::new("F001").expect("id")],
            started_at: crate::state::now_rfc3339(),
            bug_burn_floor: None,
        });
        // Hold one candidate — it must never be pulled in.
        state
            .tickets
            .iter_mut()
            .find(|t| t.id().to_string() == "F005")
            .expect("t")
            .transition_to(coxagent_domain::Role::User, Status::OnHold)
            .expect("hold");
        let n = top_up_scope(&mut state, 4);
        assert_eq!(n, 3);
        let committed = &state.sprint.as_ref().expect("sprint").committed;
        assert_eq!(committed.len(), 4);
        assert!(!committed.contains(&TicketId::new("F005").expect("id")));
        // Already at the floor: a second call is a no-op.
        assert_eq!(top_up_scope(&mut state, 4), 0);
    }

    #[test]
    fn opens_first_sprint_and_commits_backlog() {
        let mut state = ProjectState {
            tickets: vec![feature("F001"), feature("F002")],
            ..ProjectState::default()
        };
        let n = advance(&mut state, 1, SprintPolicy::cycles(10));
        assert_eq!(n, Some(1));
        let s = state.sprint.as_ref().expect("sprint");
        assert_eq!(s.number, 1);
        assert_eq!(s.committed.len(), 2);
    }

    #[test]
    fn an_empty_sprint_scope_refills_from_backlog_instead_of_starving_dev() {
        // Sprint opened onto an empty backlog…
        let mut state = ProjectState::default();
        advance(&mut state, 1, SprintPolicy::cycles(10));
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
        advance(&mut state, 1, SprintPolicy::cycles(10));
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

    // ---- CXA-F028: the bug-burn floor scopes commitment and DEV work. ----

    #[test]
    fn rollover_mirrors_the_config_floor_onto_the_new_sprint() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        let policy = SprintPolicy {
            bug_burn_floor: Some(Priority::High),
            ..SprintPolicy::cycles(10)
        };
        advance(&mut state, 1, policy);
        let s = state.sprint.as_ref().expect("sprint");
        assert_eq!(s.bug_burn_floor, Some(Priority::High));
        // Age the sprint so its window can elapse, then roll under a
        // floor-less policy: the new sprint is floor-less again.
        state.sprint.as_mut().expect("sprint").started_at = "2020-01-01T00:00:00Z".to_owned();
        advance(&mut state, 11, SprintPolicy::cycles(10));
        let s = state.sprint.as_ref().expect("sprint");
        assert_eq!(s.number, 2);
        assert_eq!(s.bug_burn_floor, None);
    }

    #[test]
    fn top_up_never_recommits_below_floor_open_bugs_mid_burn() {
        let mut state = ProjectState {
            tickets: vec![ready_feature("F001"), bug_prio("B-LOW", Priority::Low)],
            ..ProjectState::default()
        };
        state.sprint = Some(Sprint {
            number: 1,
            goal: "burn".into(),
            started_cycle: 1,
            length_cycles: 10,
            committed: vec![TicketId::new("F001").expect("id")],
            started_at: crate::state::now_rfc3339(),
            bug_burn_floor: Some(Priority::High),
        });
        // Below the MIN_ACTIONABLE_SCOPE floor the top-up would normally pull
        // the open bug in — under the burn floor it must stay parked.
        assert_eq!(top_up_scope(&mut state, MIN_ACTIONABLE_SCOPE), 0);
        assert!(!state
            .sprint
            .as_ref()
            .expect("sprint")
            .committed
            .contains(&TicketId::new("B-LOW").expect("id")));
    }

    #[test]
    fn refill_respects_the_burn_floor_when_recommitting_an_empty_scope() {
        let mut state = ProjectState {
            tickets: vec![
                bug_prio("B-HIGH", Priority::High),
                bug_prio("B-LOW", Priority::Low),
            ],
            ..ProjectState::default()
        };
        state.sprint = Some(Sprint {
            number: 1,
            goal: "burn".into(),
            started_cycle: 1,
            length_cycles: 10,
            committed: Vec::new(),
            started_at: crate::state::now_rfc3339(),
            bug_burn_floor: Some(Priority::High),
        });
        // The refill heals an empty scope, but an active burn floor means the
        // healed scope is the burn's scope: high bug in, low bug stays parked.
        assert_eq!(refill_empty_scope(&mut state), 1);
        let committed = &state.sprint.as_ref().expect("sprint").committed;
        assert!(committed.contains(&TicketId::new("B-HIGH").expect("id")));
        assert!(!committed.contains(&TicketId::new("B-LOW").expect("id")));
    }

    #[test]
    fn closing_early_keeps_the_burn_running_in_the_next_sprint() {
        let mut state = ProjectState {
            tickets: vec![bug("B001")],
            ..ProjectState::default()
        };
        let policy = SprintPolicy {
            bug_burn_floor: Some(Priority::High),
            ..SprintPolicy::cycles(10)
        };
        advance(&mut state, 1, policy);
        // An early close is a boundary write, not a burn cancel: sprint 2
        // inherits the floor (only the config changes it).
        assert_eq!(close_now(&mut state, 3), Some(2));
        assert_eq!(
            state.sprint.as_ref().expect("sprint").bug_burn_floor,
            Some(Priority::High)
        );
    }

    #[test]
    fn a_person_can_pull_a_ticket_into_the_running_sprint() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::cycles(10));
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
        advance(&mut state, 1, SprintPolicy::cycles(10));
        // Cycle 3 of a 10-cycle window: nothing would roll over on its own.
        assert_eq!(advance(&mut state, 3, SprintPolicy::cycles(10)), None);
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
        advance(&mut state, 1, SprintPolicy::cycles(10));
        assert_eq!(advance(&mut state, 5, SprintPolicy::cycles(10)), None);
        assert_eq!(state.sprint.as_ref().expect("s").number, 1);
    }

    #[test]
    fn cycles_policy_needs_both_the_counter_and_real_time() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::cycles(10));
        // Counter elapsed but the sprint is seconds old: must NOT roll. Pure
        // cycle-rolling at ~2-min cycles produced 10-minute "sprints" that
        // were all ceremony and no delivery (sprint 586, velocity 0%).
        assert_eq!(advance(&mut state, 11, SprintPolicy::cycles(10)), None);
        // Same counter, but the sprint is genuinely old — now it rolls.
        if let Some(s) = &mut state.sprint {
            s.started_at = "2020-01-01T00:00:00Z".to_owned();
        }
        assert_eq!(advance(&mut state, 11, SprintPolicy::cycles(10)), Some(2));
    }

    #[test]
    fn days_policy_ignores_cycle_count_until_the_day_passes() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::days(1));
        // A thousand cycles later but seconds old: must NOT roll — this is the
        // 500-seven-minute-sprints bug the day unit exists to kill.
        assert_eq!(advance(&mut state, 1000, SprintPolicy::days(1)), None);
        assert_eq!(state.sprint.as_ref().expect("s").number, 1);
        // Age it past a day: rolls.
        let old = (time::OffsetDateTime::now_utc() - time::Duration::hours(25))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("fmt");
        state.sprint.as_mut().expect("s").started_at = old;
        assert_eq!(advance(&mut state, 1000, SprintPolicy::days(1)), Some(2));
    }

    #[test]
    fn top_up_prefers_high_priority_then_active_milestone_work() {
        // Backlog order is adversarial: an unrelated Medium ticket comes
        // first; the PO must still pick the High ticket, then the Medium one
        // that pushes the active milestone, and leave the unrelated Medium.
        let mk = |id: &str, title: &str, prio| {
            let mut t = coxagent_domain::Ticket::new(
                TicketId::new(id).expect("id"),
                TicketType::Feature,
                title,
                "",
                prio,
                coxagent_domain::Complexity::Small,
                false,
            )
            .expect("t");
            t.set_technical_design(
                coxagent_domain::Role::Sa,
                coxagent_domain::TechnicalDesign::default(),
            )
            .expect("design");
            t.transition_to(coxagent_domain::Role::Sa, Status::Ready)
                .expect("ready");
            t
        };
        let mut state = ProjectState {
            tickets: vec![
                mk("F001", "polish the settings page", Priority::Medium),
                mk("F002", "urgent auth fix", Priority::High),
                mk(
                    "F003",
                    "sibling-project knowledge dashboard",
                    Priority::Medium,
                ),
            ],
            ..ProjectState::default()
        };
        state.milestones.push(crate::state::Milestone {
            name: "Cross-Project Knowledge Graph".into(),
            goal: "sibling-project index and knowledge dashboard".into(),
            target_version: "9.9.9".into(),
            goal_complete: false,
            fulfilled: false,
        });
        state.sprint = Some(Sprint {
            number: 1,
            goal: "g".into(),
            started_cycle: 1,
            length_cycles: 10,
            committed: Vec::new(),
            started_at: crate::state::now_rfc3339(),
            bug_burn_floor: None,
        });
        assert_eq!(top_up_scope(&mut state, 2), 2);
        let committed = &state.sprint.as_ref().expect("sprint").committed;
        assert_eq!(committed[0].to_string(), "F002", "High priority first");
        assert_eq!(
            committed[1].to_string(),
            "F003",
            "milestone-aligned Medium beats unrelated Medium"
        );
    }
}
