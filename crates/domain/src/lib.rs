//! CoXAgent domain layer — DDD core with no IO and no dependency on any other
//! CoXAgent crate. The dependency rule is enforced by Cargo: this crate cannot
//! reach outward, so business invariants stay pure and testable.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod artifact;
pub mod coverage;
pub mod debt;
pub mod error;
pub mod events;
pub mod goal;
pub mod ids;
pub mod kinds;
pub mod overview_panel;
pub mod panel;
/// Panel terminal-state contract (CXA-B192) — pure types + transition fn.
pub mod panel_terminal_state;
/// Per-panel terminal-state contract core (CXA-B252) — pure value types.
pub mod terminal_state;
pub mod test_case;
pub mod ticket;
pub mod transitions;
pub mod version;
pub mod wip_checkpoint;

pub use artifact::ArtifactVersion;
pub use coverage::{CoverageEntry, CoverageStatus};
pub use debt::{DebtSignal, DebtSignalKind};
pub use error::DomainError;
pub use events::{DesignPart, DomainEvent, EventKind};
pub use goal::{Goal, GoalStatus};
pub use ids::{GoalId, TicketId, WorkerId};
// Value objects (kind/priority/sizing/status/role) come from `kinds`; the
// aggregate and its design structs come from `ticket`.
pub use kinds::{Complexity, InterventionKind, Priority, Role, Status, TicketType};
pub use overview_panel::OverviewPanelId;
pub use panel::{
    Panel, PanelLayoutSlot, PanelListing, PanelRegistry, PanelRegistryError, PanelVisibility,
    UnknownSlot,
};
pub use ticket::{Design, TechnicalDesign, Ticket, UxDesign};
pub use version::{Bump, SemVer};
pub use wip_checkpoint::WipCheckpoint;
