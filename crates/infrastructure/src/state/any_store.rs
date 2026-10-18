//! `AnyStateStore` — a sized enum over the concrete [`StateStorePort`] adapters,
//! mirroring `AnyEngine`. Generic use cases are monomorphized over `Arc<S>`
//! where `S: StateStorePort + Sized`, so a single owning type that dispatches to
//! JSON or Postgres lets the backend be chosen at runtime without `dyn`.

use async_trait::async_trait;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::ports::outbound::{ShardKind, StateShard, WorkerCaps, WorkerEntry};
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;
use coxagent_domain::TicketId;

use super::{JsonStateStore, RestStateStore, SqlStateStore};

/// Runtime-selected state store backend.
pub enum AnyStateStore {
    Json(JsonStateStore),
    Sql(SqlStateStore),
    Rest(RestStateStore),
}

#[async_trait]
impl StateStorePort for AnyStateStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        match self {
            Self::Json(s) => s.load().await,
            Self::Sql(s) => s.load().await,
            Self::Rest(s) => s.load().await,
        }
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.save(state).await,
            Self::Sql(s) => s.save(state).await,
            Self::Rest(s) => s.save(state).await,
        }
    }

    async fn save_expecting(
        &self,
        state: &ProjectState,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.save_expecting(state, expected_revision).await,
            Self::Sql(s) => s.save_expecting(state, expected_revision).await,
            Self::Rest(s) => s.save_expecting(state, expected_revision).await,
        }
    }

    async fn current_version(&self) -> Result<Option<i64>, PortError> {
        match self {
            Self::Json(s) => s.current_version().await,
            Self::Sql(s) => s.current_version().await,
            Self::Rest(s) => s.current_version().await,
        }
    }

    // Shard ops are forwarded EXPLICITLY (CXA-C019b): left at the trait
    // defaults they would load/save the whole document through the enum even
    // when the SQL backend could serve ONE shard column — the shard-native
    // reads would silently go full-document.
    async fn load_shard(&self, kind: ShardKind) -> Result<StateShard, PortError> {
        match self {
            Self::Json(s) => s.load_shard(kind).await,
            Self::Sql(s) => s.load_shard(kind).await,
            Self::Rest(s) => s.load_shard(kind).await,
        }
    }

    async fn save_shard(&self, shard: &StateShard) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.save_shard(shard).await,
            Self::Sql(s) => s.save_shard(shard).await,
            Self::Rest(s) => s.save_shard(shard).await,
        }
    }

    async fn save_shard_expecting(
        &self,
        shard: &StateShard,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.save_shard_expecting(shard, expected_revision).await,
            Self::Sql(s) => s.save_shard_expecting(shard, expected_revision).await,
            Self::Rest(s) => s.save_shard_expecting(shard, expected_revision).await,
        }
    }

    async fn delete(&self) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.delete().await,
            Self::Sql(s) => s.delete().await,
            Self::Rest(s) => s.delete().await,
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
            Self::Rest(s) => s.claim_ticket(id, worker, now).await,
        }
    }

    async fn acquire_leader(&self, worker: &str, now: &str) -> Result<bool, PortError> {
        match self {
            Self::Json(s) => s.acquire_leader(worker, now).await,
            Self::Sql(s) => s.acquire_leader(worker, now).await,
            Self::Rest(s) => s.acquire_leader(worker, now).await,
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
            Self::Rest(s) => s.claim_stage(id, stage, worker, now).await,
        }
    }

    async fn heartbeat_worker(
        &self,
        worker: &str,
        role: &str,
        ticket: &str,
        caps: &WorkerCaps,
        now: &str,
    ) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.heartbeat_worker(worker, role, ticket, caps, now).await,
            Self::Sql(s) => s.heartbeat_worker(worker, role, ticket, caps, now).await,
            Self::Rest(s) => s.heartbeat_worker(worker, role, ticket, caps, now).await,
        }
    }

    async fn workers(&self) -> Result<Vec<WorkerEntry>, PortError> {
        match self {
            Self::Json(s) => s.workers().await,
            Self::Sql(s) => s.workers().await,
            Self::Rest(s) => s.workers().await,
        }
    }

    async fn set_desired(&self, operator: &str, running: bool) -> Result<(), PortError> {
        match self {
            Self::Json(s) => s.set_desired(operator, running).await,
            Self::Sql(s) => s.set_desired(operator, running).await,
            Self::Rest(s) => s.set_desired(operator, running).await,
        }
    }

    async fn get_desired(&self, operator: &str) -> Result<Option<bool>, PortError> {
        match self {
            Self::Json(s) => s.get_desired(operator).await,
            Self::Sql(s) => s.get_desired(operator).await,
            Self::Rest(s) => s.get_desired(operator).await,
        }
    }

    async fn acquire_operator(&self, operator: &str, instance: &str) -> Result<bool, PortError> {
        match self {
            Self::Json(s) => s.acquire_operator(operator, instance).await,
            Self::Sql(s) => s.acquire_operator(operator, instance).await,
            Self::Rest(s) => s.acquire_operator(operator, instance).await,
        }
    }
}
