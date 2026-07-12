//! The transition table and field-permission rules — pure functions, no IO.
//!
//! This is the heart of "enforce by code, not by prompt": the orchestrator can
//! only ever move a ticket along an edge listed here, performed by a role listed
//! here. Everything else is rejected at the aggregate boundary.

use crate::ticket::{Role, Status, TicketType};

/// Is `from -> to` a legal edge for this ticket type?
#[must_use]
pub fn transition_allowed(ticket_type: TicketType, from: Status, to: Status) -> bool {
    use Status::{Documented, Done, Fixed, InProgress, Open, Pending, Ready, Rejected, Verified};
    match ticket_type {
        TicketType::Feature | TicketType::Chore => matches!(
            (from, to),
            (Pending, Ready | Rejected)
                | (Ready, InProgress)
                | (InProgress, Done)
                | (Done, Documented)
        ),
        TicketType::Bug => matches!(
            (from, to),
            (Open, InProgress | Rejected) | (InProgress, Fixed) | (Fixed, Verified | Open) // Fixed -> Open = reopen after failed regression
        ),
    }
}

/// Is `actor` allowed to perform the `from -> to` transition?
///
/// `System` performs automated bookkeeping (e.g. claim = `Ready -> InProgress`)
/// and is always allowed for legal edges. `User` acts as a super-PO.
#[must_use]
pub fn can_transition(actor: Role, from: Status, to: Status) -> bool {
    use Status::{Documented, Done, Fixed, InProgress, Open, Pending, Ready, Rejected, Verified};

    if actor == Role::System {
        return true;
    }

    match (from, to) {
        // Design gate: SA/PD move a ticket to ready (aggregate re-checks DoR).
        (Pending, Ready) => matches!(actor, Role::Sa | Role::Pd),
        // PO (or a user acting as super-PO) rejects.
        (Pending | Open, Rejected) => matches!(actor, Role::Po | Role::User),
        // Claiming work is a dev action.
        (Ready | Open, InProgress) => matches!(actor, Role::DevBug | Role::DevFeature),
        // Dev completes.
        (InProgress, Done) => matches!(actor, Role::DevFeature | Role::DevBug),
        (InProgress, Fixed) => actor == Role::DevBug,
        // Test verifies / reopens.
        (Fixed, Verified | Open) => actor == Role::Test,
        // Docs marks documented.
        (Done, Documented) => actor == Role::Docs,
        _ => false,
    }
}

/// May `actor` write `field`? Field-level authority independent of status.
#[must_use]
pub fn field_permitted(actor: Role, field: &str) -> bool {
    if actor == Role::System {
        return true;
    }
    match field {
        // Priority is PO's alone (User = super-PO).
        "priority" => matches!(actor, Role::Po | Role::User),
        // SA owns technical design and dependency graph.
        "design.technical" | "depends_on" => actor == Role::Sa,
        // PD owns UX; SA may cover it when PD is disabled.
        "design.ux" => matches!(actor, Role::Pd | Role::Sa),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_happy_path_edges_are_legal() {
        let t = TicketType::Feature;
        assert!(transition_allowed(t, Status::Pending, Status::Ready));
        assert!(transition_allowed(t, Status::Ready, Status::InProgress));
        assert!(transition_allowed(t, Status::InProgress, Status::Done));
        assert!(transition_allowed(t, Status::Done, Status::Documented));
    }

    #[test]
    fn feature_cannot_skip_to_done() {
        assert!(!transition_allowed(
            TicketType::Feature,
            Status::Pending,
            Status::Done
        ));
    }

    #[test]
    fn bug_can_reopen_but_not_document() {
        assert!(transition_allowed(
            TicketType::Bug,
            Status::Fixed,
            Status::Open
        ));
        assert!(!transition_allowed(
            TicketType::Bug,
            Status::Done,
            Status::Documented
        ));
    }

    #[test]
    fn only_sa_pd_open_the_design_gate() {
        assert!(can_transition(Role::Sa, Status::Pending, Status::Ready));
        assert!(can_transition(Role::Pd, Status::Pending, Status::Ready));
        assert!(!can_transition(
            Role::DevFeature,
            Status::Pending,
            Status::Ready
        ));
    }

    #[test]
    fn dev_cannot_touch_priority() {
        assert!(!field_permitted(Role::DevFeature, "priority"));
        assert!(field_permitted(Role::Po, "priority"));
        assert!(field_permitted(Role::User, "priority"));
    }

    #[test]
    fn system_is_omnipotent_over_legal_edges() {
        assert!(can_transition(
            Role::System,
            Status::Ready,
            Status::InProgress
        ));
        assert!(field_permitted(Role::System, "priority"));
    }
}
