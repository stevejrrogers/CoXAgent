//! `RecoverUseCase` — startup recovery. A run that crashed mid-cycle leaves
//! tickets stuck `InProgress` (claimed but unfinished). At daemon start we
//! release those claims back to the queue so the work is picked up again.
//! Because recovery lives at the top of the loop, a crash never loses work.

use crate::error::AppError;
use crate::ports::outbound::StateStorePort;
use coxagent_domain::{Role, Status, TicketId};
use std::sync::Arc;

/// Releases orphaned `InProgress` claims at startup.
pub struct RecoverUseCase<S: StateStorePort> {
    store: Arc<S>,
}

impl<S: StateStorePort> RecoverUseCase<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// Release every `InProgress` ticket back to its queue. Returns the ids that
    /// were recovered.
    ///
    /// # Errors
    /// [`AppError`] on load/save failure.
    pub async fn execute(&self) -> Result<Vec<TicketId>, AppError> {
        let mut state = self.store.load().await?;
        let orphaned: Vec<TicketId> = state
            .tickets
            .iter()
            .filter(|t| t.status() == Status::InProgress)
            .map(|t| t.id().clone())
            .collect();

        if orphaned.is_empty() {
            return Ok(Vec::new());
        }
        for id in &orphaned {
            if let Some(ticket) = state.ticket_mut(id) {
                ticket.release_claim(Role::System)?;
            }
        }
        self.store.save(&state).await?;
        Ok(orphaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ProjectState;
    use crate::PortError;
    use coxagent_domain::{Complexity, Priority, TechnicalDesign, Ticket, TicketType};
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }
    #[async_trait::async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
            s.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = s.clone();
            Ok(())
        }
    }

    fn in_progress_feature(id: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t");
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("d");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.transition_to(Role::DevFeature, Status::InProgress)
            .expect("claim");
        t
    }

    #[tokio::test]
    async fn releases_orphaned_claims_back_to_ready() {
        let store = Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                tickets: vec![in_progress_feature("FEAT-001")],
                ..ProjectState::default()
            }),
        });
        let recovered = RecoverUseCase::new(Arc::clone(&store))
            .execute()
            .await
            .expect("recover");
        assert_eq!(recovered.len(), 1);
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Ready);
    }

    #[tokio::test]
    async fn nothing_to_recover_is_noop() {
        let store = Arc::new(MemStore::default());
        assert!(RecoverUseCase::new(store)
            .execute()
            .await
            .expect("recover")
            .is_empty());
    }
}
