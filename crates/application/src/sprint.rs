//! Sprint layer for scrum mode. Pure over state — the cycle calls `advance`
//! at the start of each cycle; it opens the first sprint and rolls over to a
//! new one when the window elapses, committing the open feature backlog.

use crate::state::{ProjectState, Sprint};
use coxagent_domain::{Status, TicketId, TicketType};

/// Open the first sprint or roll over an elapsed one. Returns the number of a
/// newly opened sprint, or `None` when the current sprint is still running.
pub fn advance(state: &mut ProjectState, cycle: u64, length: u64) -> Option<u32> {
    let length = length.max(1);
    let need_open = match &state.sprint {
        None => true,
        Some(s) => cycle.saturating_sub(s.started_cycle) >= length,
    };
    if !need_open {
        return None;
    }
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
    });
    Some(number)
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

/// Feature/chore tickets not yet shipped — the work a sprint commits to.
fn open_backlog(state: &ProjectState) -> Vec<TicketId> {
    let ready: Vec<TicketId> = state
        .tickets
        .iter()
        .filter(|t| {
            matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
                && !matches!(
                    t.status(),
                    Status::Done | Status::Documented | Status::Rejected
                )
        })
        .map(|t| t.id().clone())
        .collect();
    ready.into_iter().take(sprint_capacity(state)).collect()
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
        let n = advance(&mut state, 1, 10);
        assert_eq!(n, Some(1));
        let s = state.sprint.as_ref().expect("sprint");
        assert_eq!(s.number, 1);
        assert_eq!(s.committed.len(), 2);
    }

    #[test]
    fn does_not_reopen_mid_sprint() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, 10);
        assert_eq!(advance(&mut state, 5, 10), None);
        assert_eq!(state.sprint.as_ref().expect("s").number, 1);
    }

    #[test]
    fn rolls_over_after_length() {
        let mut state = ProjectState {
            tickets: vec![feature("F001")],
            ..ProjectState::default()
        };
        advance(&mut state, 1, 10);
        // cycle 11 is 10 cycles after start -> roll over.
        assert_eq!(advance(&mut state, 11, 10), Some(2));
        assert_eq!(state.sprint.as_ref().expect("s").number, 2);
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
