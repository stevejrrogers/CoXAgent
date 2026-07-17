//! `AnyStateStore` — a sized enum over the concrete [`StateStorePort`] adapters,
//! mirroring `AnyEngine`. Generic use cases are monomorphized over `Arc<S>`
//! where `S: StateStorePort + Sized`, so a single owning type that dispatches to
//! JSON or Postgres lets the backend be chosen at runtime without `dyn`.

use async_trait::async_trait;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::ports::outbound::WorkerEntry;
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;
use coxagent_domain::TicketId;

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

    async fn claim_ticket(
        &self,
        id: &TicketId,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
        match self {
            Self::Json(s) => s.claim_ticket(id, worker, now).await,
            Self::Sql(s) => s.claim_ticket(id, worker, now).await,
        }
    }

    async fn acquire_leader(&self, worker: &str, now: &str) -> Result<bool, PortError> {
        match self {
            Self::Json(s) => s.acquire_leader(worker, now).await,
            Self::Sql(s) => s.acquire_leader(worker, now).await,
        }
    }

    async fn claim_stage(
        &self,
        id: &TicketId,
        stage: &str,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
        match self {
            Self::Json(s) => s.claim_stage(id, stage, worker, now).await,
            Self::Sql(s) => s.claim_stage(id, stage, worker, now).await,
        }
    }

    async fn heartbeat_worker(
        &self,
        worker: &str,
        role: &str,
        ticket: &str,
        now: &str,
    ) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.heartbeat_worker(worker, role, ticket, now).await,
            Self::Sql(s) => s.heartbeat_worker(worker, role, ticket, now).await,
        }
    }

    async fn workers(&self) -> Result<Vec<WorkerEntry>, PortError> {
        match self {
            Self::Json(s) => s.workers().await,
            Self::Sql(s) => s.workers().await,
        }
    }

    async fn set_desired(&self, operator: &str, running: bool) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.set_desired(operator, running).await,
            Self::Sql(s) => s.set_desired(operator, running).await,
        }
    }

    async fn get_desired(&self, operator: &str) -> Result<Option<bool>, PortError> {
        match self {
            Self::Json(s) => s.get_desired(operator).await,
            Self::Sql(s) => s.get_desired(operator).await,
        }
    }
}
