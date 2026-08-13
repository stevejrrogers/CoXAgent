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
            SprintPolicy::Cycles(len) => cycle.saturating_sub(s.started_cycle) >= len,
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
    let committed = open_backlog(state);
    // The PO's set goal wins; otherwise derive one from the committed titles.
    let goal = if state.sprint_goal.trim().is_empty() {
        goal_from(state, &committed)
    } else {
        state.sprint_goal.trim().to_owned()
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
    let mut picked: Vec<TicketId> = state
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
        .map(|t| t.id().clone())
        .collect();
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
            .map(|t| t.id().clone()),
    );
    picked.into_iter().take(sprint_capacity(state)).collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Complexity, Priority, Ticket, TicketType};

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
    fn cycles_policy_rolls_purely_on_cycles() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, SprintPolicy::Cycles(10));
        // The explicit cycles unit is a deliberate choice — it rolls on the
        // counter alone, however fast cycles spin.
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
