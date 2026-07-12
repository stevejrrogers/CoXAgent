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
    let number = state.sprint.as_ref().map_or(0, |s| s.number) + 1;
    let committed = open_backlog(state);
    state.sprint = Some(Sprint {
        number,
        goal: goal_from(state, &committed),
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
        .map(|t| t.id().clone())
        .collect()
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
