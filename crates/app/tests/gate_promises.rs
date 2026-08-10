//! Repo-wide guard that every HUMAN-GATE promise the endpoints make is backed
//! by a legal domain transition — the "wired?" class of bug this suite exists
//! to kill.
//!
//! The inbox/chat endpoints (`human_ready_ep`, `human_verify_ep`, `reject`,
//! `undo_approval_ep`, `send_back_ep`) each move a ticket along ONE domain edge
//! as `Role::User`. When such an edge is missing from the transition table the
//! endpoint compiles, ships, and answers `409 invalid transition` at runtime —
//! exactly how the adaptive gate's 30-minute Undo was dead on arrival
//! (`Ready -> Pending` was never a legal edge for a feature). A unit test on the
//! endpoint would not catch it; the promise only breaks where the endpoint's
//! move meets the table. So assert that contract directly, here.

use coxagent_domain::transitions::{can_transition, transition_allowed};
use coxagent_domain::{Role, Status, TicketType};

/// One promise: an endpoint that moves a ticket of `types` along `from -> to`
/// as the human operator.
struct GatePromise {
    endpoint: &'static str,
    from: Status,
    to: Status,
    types: &'static [TicketType],
}

/// The full set of human-gate moves the server exposes. Keep this in lockstep
/// with the endpoints in `server/inbox.rs` + `server/work.rs`: add an edge here
/// the day you add a gate action, and this test tells you if the table forgot
/// to allow it.
const PROMISES: &[GatePromise] = &[
    // Approve to Ready — the BA/PO gate (features and chores are designed).
    GatePromise {
        endpoint: "human_ready_ep (approve → Ready)",
        from: Status::Pending,
        to: Status::Ready,
        types: &[TicketType::Feature, TicketType::Chore],
    },
    // Reject a designed ticket, or one already promoted to Ready.
    GatePromise {
        endpoint: "reject_ticket (Pending → Rejected)",
        from: Status::Pending,
        to: Status::Rejected,
        types: &[TicketType::Feature, TicketType::Chore],
    },
    GatePromise {
        endpoint: "reject_ticket / undo-as-reject (Ready → Rejected)",
        from: Status::Ready,
        to: Status::Rejected,
        types: &[TicketType::Feature, TicketType::Chore],
    },
    // Undo an auto-approval inside the window — the edge that was missing.
    GatePromise {
        endpoint: "undo_approval_ep (Ready → Pending)",
        from: Status::Ready,
        to: Status::Pending,
        types: &[TicketType::Feature, TicketType::Chore],
    },
    // Verify a fixed bug — the QA gate.
    GatePromise {
        endpoint: "human_verify_ep (Fixed → Verified)",
        from: Status::Fixed,
        to: Status::Verified,
        types: &[TicketType::Bug],
    },
    // Send a fix back for lack of evidence — the verify gate's other answer.
    GatePromise {
        endpoint: "send_back_ep (Fixed → Open)",
        from: Status::Fixed,
        to: Status::Open,
        types: &[TicketType::Bug],
    },
];

#[test]
fn every_human_gate_edge_is_a_legal_transition() {
    for p in PROMISES {
        // The human always acts as Role::User.
        assert!(
            can_transition(Role::User, p.from, p.to),
            "{}: User may not move {:?} -> {:?} — the endpoint would 409",
            p.endpoint,
            p.from,
            p.to
        );
        for &t in p.types {
            assert!(
                transition_allowed(t, p.from, p.to),
                "{}: {:?} has no {:?} -> {:?} edge — the gate is dead for this type",
                p.endpoint,
                t,
                p.from,
                p.to
            );
        }
    }
}

/// The undo window's exact regression: it promises a takeback BEFORE work
/// starts, and must NOT let a human rewind work already underway.
#[test]
fn undo_reaches_back_only_before_work_starts() {
    for t in [TicketType::Feature, TicketType::Chore] {
        assert!(
            transition_allowed(t, Status::Ready, Status::Pending),
            "undo must return a not-yet-started {t:?} to Pending"
        );
        assert!(
            !transition_allowed(t, Status::InProgress, Status::Pending),
            "a {t:?} already InProgress must not be silently un-approved"
        );
    }
}

/// A developer role must never be able to take a human gate decision — the
/// authority to approve/verify is the human's (or the agent role that owns the
/// automated edge), never a passing writer.
#[test]
fn a_developer_cannot_take_a_human_gate_decision() {
    assert!(!can_transition(Role::DevFeature, Status::Ready, Status::Pending));
    assert!(!can_transition(Role::DevBug, Status::Fixed, Status::Verified));
    assert!(!can_transition(Role::DevFeature, Status::Pending, Status::Rejected));
}
