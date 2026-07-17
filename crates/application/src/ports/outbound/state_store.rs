//! `StateStorePort` — the repository boundary. `JsonStateStore` implements it
//! locally; a `RemoteStateStore` (hub) implements it later without touching use
//! cases. This trait is why local -> team is an adapter swap, not a rewrite.

use crate::error::PortError;
use crate::state::ProjectState;
use async_trait::async_trait;
use coxagent_domain::{Role, TicketId};
use serde::{Deserialize, Serialize};

/// A live entry in the cross-machine worker registry: which runner (`account@
/// host`) is online, whether it currently leads, and what it last reported doing.
/// Lets any dashboard show every team working the project, even on other hosts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEntry {
    pub worker: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ticket: String,
    pub at: String,
}

/// Atomic read-modify-write with retry: load the state, apply `f`, and save. If
/// a concurrent writer advanced the revision (a [`PortError::Conflict`]), reload
/// and re-apply `f` up to a bounded number of times. This is how two operators
/// working the same project in parallel both persist their changes without one
/// silently clobbering the other or losing work — `f` re-runs on a fresh state
/// each retry, so it must recompute from the reloaded state (not capture stale
/// values).
///
/// # Errors
/// The mutator's error, or [`PortError::Conflict`] if it never converged.
pub async fn mutate_state<S, F>(store: &S, mut f: F) -> Result<(), PortError>
where
    S: StateStorePort + ?Sized,
    F: FnMut(&mut ProjectState) -> Result<(), PortError>,
{
    let mut last = PortError::Conflict("mutate: no attempts".to_owned());
    for _ in 0..12 {
        let mut state = store.load().await?;
        f(&mut state)?;
        match store.save(&state).await {
            Ok(()) => return Ok(()),
            Err(PortError::Conflict(e)) => last = PortError::Conflict(e),
            Err(other) => return Err(other),
        }
    }
    Err(last)
}

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

    /// Try to become (or renew) the project's work leader for singleton phases
    /// (BA proposals, milestones, review/merge) that must run once, not once per
    /// runner. `true` means the caller holds leadership this cycle.
    ///
    /// The default always grants it — a single runner is always the leader.
    /// Shared backends override this with a lease so exactly one of several
    /// concurrent runners leads at a time (with takeover if the leader dies).
    ///
    /// # Errors
    /// [`PortError`] on a coordination-store failure.
    async fn acquire_leader(&self, _worker: &str, _now: &str) -> Result<bool, PortError> {
        Ok(true)
    }

    /// Atomically claim a per-ticket work `stage` (e.g. `"sa"`, `"pd"`, `"docs"`)
    /// for `worker`, so two concurrent runners never do the same stage on the
    /// same ticket. `true` means the caller won the stage.
    ///
    /// The default always grants it (single runner). Shared backends override
    /// with a lease keyed by `(ticket, stage)`.
    ///
    /// # Errors
    /// [`PortError`] on a coordination-store failure.
    async fn claim_stage(
        &self,
        _id: &TicketId,
        _stage: &str,
        _worker: &str,
        _now: &str,
    ) -> Result<bool, PortError> {
        Ok(true)
    }

    /// Record this runner's live presence (`account@host`, current role, current
    /// ticket) in the shared worker registry, so every dashboard can show all
    /// teams. Best-effort; the default is a no-op (single-runner needs none).
    ///
    /// # Errors
    /// [`PortError`] on a coordination-store failure.
    async fn heartbeat_worker(
        &self,
        _worker: &str,
        _role: &str,
        _ticket: &str,
        _now: &str,
    ) -> Result<(), PortError> {
        Ok(())
    }

    /// The registry of workers seen alive recently (stale entries pruned). The
    /// default is empty (single-runner shows only its own live snapshot).
    ///
    /// # Errors
    /// [`PortError`] on a coordination-store failure.
    async fn workers(&self) -> Result<Vec<WorkerEntry>, PortError> {
        Ok(Vec::new())
    }

    /// Persist an operator's desired run state (`true` = should be running) so a
    /// user's start/stop intent survives restarts and drives auto-resume when
    /// that same operator reopens the app. Per-operator, so one user's choice
    /// never starts or stops another's. Default no-op (single-runner file store).
    ///
    /// # Errors
    /// [`PortError`] on a coordination-store failure.
    async fn set_desired(&self, _operator: &str, _running: bool) -> Result<(), PortError> {
        Ok(())
    }

    /// This operator's persisted desired run state, or `None` if never set (in
    /// which case the runner stays idle until an explicit start).
    ///
    /// # Errors
    /// [`PortError`] on a coordination-store failure.
    async fn get_desired(&self, _operator: &str) -> Result<Option<bool>, PortError> {
        Ok(None)
    }

    /// Acquire or renew a single-instance lock for `operator`, held by `instance`
    /// (a per-process token such as the PID). Returns `true` if this instance
    /// holds the lock; `false` means another live process already runs this
    /// operator, so the caller should not start a duplicate. Default `true`
    /// (single-machine file store needs no cross-process lock).
    ///
    /// # Errors
    /// [`PortError`] on a coordination-store failure.
    async fn acquire_operator(&self, _operator: &str, _instance: &str) -> Result<bool, PortError> {
        Ok(true)
    }
}
