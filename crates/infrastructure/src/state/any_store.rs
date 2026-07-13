//! `AnyStateStore` — a sized enum over the concrete [`StateStorePort`] adapters,
//! mirroring `AnyEngine`. Generic use cases are monomorphized over `Arc<S>`
//! where `S: StateStorePort + Sized`, so a single owning type that dispatches to
//! JSON or Postgres lets the backend be chosen at runtime without `dyn`.

use async_trait::async_trait;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;

use super::{JsonStateStore, SqlStateStore};

/// Runtime-selected state store backend.
pub enum AnyStateStore {
    Json(JsonStateStore),
    Sql(SqlStateStore),
}

#[async_trait]
impl StateStorePort for AnyStateStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        match self {
            Self::Json(s) => s.load().await,
            Self::Sql(s) => s.load().await,
        }
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.save(state).await,
            Self::Sql(s) => s.save(state).await,
        }
    }
}
