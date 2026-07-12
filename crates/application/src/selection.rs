//! Pure ticket-selection policy. The orchestrator asks "what's next?"; these
//! functions answer deterministically from state, so the choice is testable
//! without any engine. Kept separate from the loop for the same reason.

use crate::state::ProjectState;
use coxagent_domain::{Priority, Status, Ticket, TicketId, TicketType};

/// Highest-priority open bug (critical work first). Ties broken by id order for
/// determinism.
#[must_use]
pub fn next_open_bug(state: &ProjectState) -> Option<TicketId> {
    best(state, |t| {
        t.ticket_type() == TicketType::Bug && t.status() == Status::Open
    })
}

/// Highest-priority `ready` feature whose dependencies are all satisfied.
#[must_use]
pub fn next_ready_feature(state: &ProjectState) -> Option<TicketId> {
    best(state, |t| {
        matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
            && t.status() == Status::Ready
            && deps_satisfied(state, t)
    })
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

/// Pick the highest-priority ticket matching `pred`, breaking ties by id.
fn best<F: Fn(&Ticket) -> bool>(state: &ProjectState, pred: F) -> Option<TicketId> {
    state
        .tickets
        .iter()
        .filter(|t| pred(t))
        .max_by(|a, b| {
            priority_rank(a.priority())
                .cmp(&priority_rank(b.priority()))
                .then_with(|| b.id().as_str().cmp(a.id().as_str()))
        })
        .map(|t| t.id().clone())
}

fn priority_rank(p: Priority) -> u8 {
    match p {
        Priority::Low => 0,
        Priority::Medium => 1,
        Priority::High => 2,
    }
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
}
