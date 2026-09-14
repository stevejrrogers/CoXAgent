//! `ArchiveStorePort` — the cold store holding tickets evicted from a
//! project's hot [`crate::state::ProjectState`] (CXA-F272/F273/F274).
//!
//! The write side (eviction) lands tickets here and drops them from the hot
//! state in one store save; the read back (this ticket, F274) serves them to
//! the board UI and the detail endpoint so archival stays invisible to users.
//! Everything is keyed by project id, exactly like [`super::DocStorePort`].

use crate::error::PortError;
use async_trait::async_trait;
use coxagent_domain::ticket::Ticket;

/// Cold persistence for archived (evicted) tickets, keyed by project id.
#[async_trait]
pub trait ArchiveStorePort: Send + Sync {
    /// Persist one evicted ticket (idempotent upsert by `ticket.id()`).
    async fn put(&self, project: &str, ticket: &Ticket) -> Result<(), PortError>;

    /// One archived ticket by id, or `None` when the archive does not hold it.
    async fn get(&self, project: &str, id: &str) -> Result<Option<Ticket>, PortError>;

    /// Every archived ticket for a project. Order is not implied — readers
    /// sort (the board read model orders id-descending).
    async fn list(&self, project: &str) -> Result<Vec<Ticket>, PortError>;
}
