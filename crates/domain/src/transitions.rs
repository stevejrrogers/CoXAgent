//! The transition table and field-permission rules — pure functions, no IO.
//!
//! This is the heart of "enforce by code, not by prompt": the orchestrator can
//! only ever move a ticket along an edge listed here, performed by a role listed
//! here. Everything else is rejected at the aggregate boundary.

use crate::kinds::{Role, Status, TicketType};

/// Is `from -> to` a legal edge for this ticket type?
#[must_use]
pub fn transition_allowed(ticket_type: TicketType, from: Status, to: Status) -> bool {
    use Status::{Documented, Done, Fixed, InProgress, Open, Pending, Ready, Rejected, Verified};
    match ticket_type {
        TicketType::Feature | TicketType::Chore => matches!(
            (from, to),
            (Pending, Ready | Rejected)
                // Ready -> Pending = an approval taken back before any work
                // started. The adaptive gate promises a 30-minute undo window
                // on everything it auto-approves; without this edge that
                // promise could not be kept for a feature or a chore — the
                // endpoint answered "invalid transition" and the ticket stayed
                // approved. Ready -> Rejected likewise: a duplicate spotted one
                // minute too late had no way back off the board.
                | (Ready, InProgress | Pending | Rejected)
                | (InProgress, Done)
                | (Done, Documented)
        ),
        TicketType::Bug => matches!(
            (from, to),
            // Open -> Rejected = filed in error / duplicate caught before work.
            // InProgress -> Rejected = the automated not-reproducible close: a
            // bug pass that ends with no reproduction on a green tree (or is a
            // duplicate already fixed under another ticket) must be able to
            // CLOSE instead of re-queue. Historically that mid-work close did
            // not exist, so the phantom-bug guard's System Rejected transition
            // failed, the ticket bounced back into the queue, and a
            // not-reproducible bug burned a fresh full investigation every
            // sprint (the CXA-B002/B003/B004 loop).
            (Open, InProgress | Rejected)
                | (InProgress, Fixed | Rejected)
                | (Fixed, Verified | Open) // Fixed -> Open = reopen after failed regression
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
        // A human (dashboard user) approves readiness when the hybrid
        // ready-gate is on — same move, person instead of agent.
        (Pending, Ready) => matches!(actor, Role::Sa | Role::Pd | Role::User),
        // PO (or a user acting as super-PO) rejects. From `Ready` too: work has
        // not started there, and a duplicate is worth catching late.
        (Pending | Open | Ready, Rejected) => matches!(actor, Role::Po | Role::User),
        // Taking an approval back — the undo window, and only before work starts.
        (Ready, Pending) => matches!(actor, Role::Po | Role::User),
        // Claiming work is a dev action.
        (Ready | Open, InProgress) => matches!(actor, Role::DevBug | Role::DevFeature),
        // Dev completes.
        (InProgress, Done) => matches!(actor, Role::DevFeature | Role::DevBug),
        (InProgress, Fixed) => actor == Role::DevBug,
        // Test verifies / reopens.
        // Human QA renders the verdict when the hybrid verify-gate is on.
        (Fixed, Verified | Open) => matches!(actor, Role::Test | Role::User),
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
        // Pre-mortem findings are written by the automated analysis run after
        // an agent or heuristic pass — never by a human or a DEV directly.
        "pre_mortem" => actor == Role::System,
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
    fn bug_in_progress_can_be_rejected_by_system() {
        // The phantom-bug guard closes a non-reproducible / duplicate bug
        // mid-work via the System role. InProgress -> Rejected must be a legal
        // bug edge or that close fails and the ticket loops forever.
        assert!(transition_allowed(
            TicketType::Bug,
            Status::InProgress,
            Status::Rejected
        ));
        assert!(can_transition(
            Role::System,
            Status::InProgress,
            Status::Rejected
        ));
        // DEV does NOT get the close: the guard deliberately routes the
        // not-reproducible verdict through System so a dev can't self-close
        // and burn through the sprint.
        assert!(!can_transition(
            Role::DevBug,
            Status::InProgress,
            Status::Rejected
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

#[cfg(test)]
mod undo_window_tests {
    use super::{can_transition, transition_allowed};
    use crate::kinds::{Role, Status, TicketType};

    #[test]
    fn an_approval_can_be_taken_back_before_work_starts() {
        // The adaptive gate promises a 30-minute undo on everything it
        // auto-approves. Without this edge the endpoint answered "invalid
        // transition for Feature: Ready -> Pending" and the promise was empty.
        for t in [TicketType::Feature, TicketType::Chore] {
            assert!(transition_allowed(t, Status::Ready, Status::Pending));
            assert!(transition_allowed(t, Status::Ready, Status::Rejected));
        }
        assert!(can_transition(Role::User, Status::Ready, Status::Pending));
        assert!(can_transition(Role::Po, Status::Ready, Status::Rejected));
        // Not a developer's call: they claim work, they do not un-approve it.
        assert!(!can_transition(
            Role::DevFeature,
            Status::Ready,
            Status::Pending
        ));
    }

    #[test]
    fn work_already_underway_is_not_undone_by_the_gate() {
        assert!(!transition_allowed(
            TicketType::Feature,
            Status::InProgress,
            Status::Pending
        ));
    }
}
