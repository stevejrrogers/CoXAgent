//! CXA-F415a — one lockdown reproduction per high-severity bug, named by its
//! real hub ticket id. Each file carries a `// LOCKDOWN: CXA-<ticket>` header;
//! ledger integrity is enforced by crates/app/tests/f415_ledger_gate.rs.
//! This subtask ships NO production behaviour change.
#![allow(clippy::unwrap_used, clippy::expect_used)]

pub mod b_myp_b001;
pub mod b_myp_b002;
pub mod b_myp_b003;
pub mod b_myp_b004;
pub mod b_myp_b005;
pub mod b_myp_b006;
pub mod b_myp_b007;
pub mod b_myp_b008;

use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};

use coxagent_application::state::{ProjectState, Tuning};
use time::format_description::well_known::Rfc3339;

/// A fresh high/low-priority bug. `Ticket::new` already creates bugs in
/// `Status::Open` (same convention as selection.rs's own test helpers), so no
/// transition is needed — the aggregate guards that edge.
/// # Panics
/// Panics only on a malformed fixture id (`F415-*` literals are valid).
#[must_use]
pub fn open_bug(id: &str, prio: Priority) -> Ticket {
    Ticket::new(
        TicketId::new(id).expect("valid id"),
        TicketType::Bug,
        "CXA-F415a regression fixture",
        "",
        prio,
        Complexity::Small,
        false,
    )
    .expect("bug ticket")
}

/// RFC3339 stamp helper used by burn-deadline fixtures.
/// # Panics
/// Panics only on an out-of-range timestamp (fixtures use fixed safe values).
#[must_use]
pub fn rfc3339_at(unix_secs: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix_secs)
        .expect("valid timestamp")
        .format(&Rfc3339)
        .expect("rfc3339 format")
}

/// A board with burn mode armed on `committed`, deadline at `deadline_secs`,
/// carrying exactly the named tickets.
#[must_use]
pub fn burn_board(committed: &[&str], deadline_secs: i64, tickets: Vec<Ticket>) -> ProjectState {
    let tuning = Tuning {
        burn_mode: true,
        committed_bug_ids: committed.iter().map(|s| (*s).to_owned()).collect(),
        burn_deadline: Some(rfc3339_at(deadline_secs)),
        ..Tuning::default()
    };
    ProjectState {
        tickets,
        tuning,
        ..ProjectState::default()
    }
}
