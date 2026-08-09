//! CoXAgent domain layer — DDD core with no IO and no dependency on any other
//! CoXAgent crate. The dependency rule is enforced by Cargo: this crate cannot
//! reach outward, so business invariants stay pure and testable.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod debt;
pub mod error;
pub mod events;
pub mod ids;
pub mod ticket;
pub mod transitions;
pub mod version;

pub use debt::{DebtSignal, DebtSignalKind};
pub use error::DomainError;
pub use events::{DesignPart, DomainEvent, EventKind};
pub use ids::{TicketId, WorkerId};
pub use ticket::{
    Complexity, Design, Priority, Role, Status, TechnicalDesign, Ticket, TicketType, UxDesign,
};
pub use version::{Bump, SemVer};
