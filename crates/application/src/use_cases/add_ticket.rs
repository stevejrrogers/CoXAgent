//! `AddTicketUseCase` — the first vertical slice: load state, append a validated
//! ticket, persist atomically. Demonstrates the port wiring end to end.

use crate::error::AppError;
use crate::ports::outbound::StateStorePort;
use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};
use std::sync::Arc;

/// Input for adding a ticket. Id minting lives here for now (per-type prefix).
pub struct AddTicketInput {
    pub ticket_type: TicketType,
    pub title: String,
    pub description: String,
    pub priority: Priority,
    pub complexity: Complexity,
    pub has_ui: bool,
}

/// Adds a new ticket to the project backlog.
pub struct AddTicketUseCase<S: StateStorePort> {
    store: Arc<S>,
}

impl<S: StateStorePort> AddTicketUseCase<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// Execute the use case, returning the id of the created ticket.
    ///
    /// # Errors
    /// - [`AppError::Domain`] when the ticket fails construction.
    /// - [`AppError::Port`] when load/save fails or the result is invalid.
    pub async fn execute(&self, input: AddTicketInput) -> Result<TicketId, AppError> {
        let mut state = self.store.load().await?;

        let id = mint_id(input.ticket_type, &state)?;
        let ticket = Ticket::new(
            id.clone(),
            input.ticket_type,
            input.title,
            input.description,
            input.priority,
            input.complexity,
            input.has_ui,
        )?;
        state.tickets.push(ticket);

        state.validate().map_err(crate::error::PortError::Corrupt)?;
        self.store.save(&state).await?;
        Ok(id)
    }
}

/// Mint the next id for a type by counting existing tickets of that prefix.
///
/// # Errors
/// Propagates [`DomainError`] from id construction (unreachable in practice
/// since the formatted string is always non-empty, but kept honest).
fn mint_id(
    ticket_type: TicketType,
    state: &crate::state::ProjectState,
) -> Result<TicketId, AppError> {
    let prefix = match ticket_type {
        TicketType::Feature => "FEAT",
        TicketType::Bug => "BUG",
        TicketType::Chore => "CHORE",
    };
    let next = state
        .tickets
        .iter()
        .filter(|t| t.id().as_str().starts_with(prefix))
        .count()
        + 1;
    Ok(TicketId::new(format!("{prefix}-{next:03}"))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::StateStorePort;
    use crate::state::ProjectState;
    use crate::PortError;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// In-memory store — lets us unit-test the use case with no filesystem.
    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }

    #[async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().map_err(poison)?.clone())
        }
        async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
            state.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().map_err(poison)? = state.clone();
            Ok(())
        }
    }

    fn poison<T>(_: std::sync::PoisonError<T>) -> PortError {
        PortError::Backend("lock poisoned".to_owned())
    }

    fn input(ticket_type: TicketType) -> AddTicketInput {
        AddTicketInput {
            ticket_type,
            title: "T".to_owned(),
            description: String::new(),
            priority: Priority::Medium,
            complexity: Complexity::Small,
            has_ui: false,
        }
    }

    #[tokio::test]
    async fn mints_sequential_ids_per_type() {
        let store = Arc::new(MemStore::default());
        let uc = AddTicketUseCase::new(Arc::clone(&store));

        let a = uc.execute(input(TicketType::Feature)).await.expect("a");
        let b = uc.execute(input(TicketType::Feature)).await.expect("b");
        let c = uc.execute(input(TicketType::Bug)).await.expect("c");

        assert_eq!(a.as_str(), "FEAT-001");
        assert_eq!(b.as_str(), "FEAT-002");
        assert_eq!(c.as_str(), "BUG-001");
        assert_eq!(store.load().await.expect("load").tickets.len(), 3);
    }

    #[tokio::test]
    async fn rejects_blank_title() {
        let store = Arc::new(MemStore::default());
        let uc = AddTicketUseCase::new(store);
        let mut bad = input(TicketType::Feature);
        bad.title = "   ".to_owned();
        assert!(uc.execute(bad).await.is_err());
    }
}
