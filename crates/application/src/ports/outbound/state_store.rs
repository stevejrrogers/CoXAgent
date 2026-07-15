//! `StateStorePort` — the repository boundary. `JsonStateStore` implements it
//! locally; a `RemoteStateStore` (hub) implements it later without touching use
//! cases. This trait is why local -> team is an adapter swap, not a rewrite.

use crate::error::PortError;
use crate::state::ProjectState;
use async_trait::async_trait;
use coxagent_domain::{Role, TicketId};

/// Persistence port for the project aggregate.
///
/// Implementations must make `save` atomic (no torn writes) and guard against
/// concurrent writers.
#[async_trait]
pub trait StateStorePort: Send + Sync {
    /// Load the full state, or the default when nothing has been persisted yet.
    async fn load(&self) -> Result<ProjectState, PortError>;

    /// Persist the full state atomically after validating it.
    async fn save(&self, state: &ProjectState) -> Result<(), PortError>;

    /// Atomically claim `id` for `worker` (`account@host`), stamping `now` as the
    /// lease time. Returns `true` if this caller won the claim, `false` if the
    /// ticket is missing, already claimed, or not in a claimable state.
    ///
    /// The default is a best-effort load/check/save — correct for a single
    /// runner. Backends shared by concurrent runners (e.g. [`JsonStateStore`])
    /// override this with a locked, cross-process-atomic critical section so two
    /// workers can never win the same ticket.
    ///
    /// # Errors
    /// [`PortError`] on a load or save failure.
    async fn claim_ticket(
        &self,
        id: &TicketId,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
        let mut state = self.load().await?;
        let Some(ticket) = state.ticket_mut(id) else {
            return Ok(false);
        };
        if ticket.claimed_by().is_some() {
            return Ok(false);
        }
        if ticket.claim(Role::System, worker, now).is_err() {
            return Ok(false);
        }
        self.save(&state).await?;
        Ok(true)
    }
}
