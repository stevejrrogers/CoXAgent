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
    /// Agent CLIs this runner found on ITS OWN PATH (`claude`, `opencode`, …).
    ///
    /// The hub cannot infer this: in a split deploy the dashboard is served by a
    /// container that will never have an agent CLI, while the agents run on an
    /// operator's machine. Detecting locally there made the dashboard report "no
    /// agent CLI detected" and mark every engine "(not installed)" while those
    /// engines were in fact running the team. Each runner reports what it has,
    /// and the entry expires with the heartbeat — so the list tracks who is
    /// actually online rather than what was once installed somewhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<String>,
    /// `provider/model` pairs this runner's opencode can reach. A user's custom
    /// providers exist only in their own opencode config, so no built-in list
    /// can name them and the hub has no CLI to ask — the machine that has one
    /// reports them here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// What this runner's git and forge credentials can actually do, probed on
    /// ITS machine. `None` until it has reported (or when git is disabled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitCheck>,
    /// The developer tooling (git/gh/glab/docker) present on THIS runner's
    /// machine, and that machine's OS — serialized `Tooling`.
    ///
    /// The hub cannot answer either: `std::env::consts::OS` is the OS of
    /// whatever process asks, so a container reported `linux` and offered
    /// `apt-get install` to someone on a Mac, then marked git, gh and docker
    /// missing while all three sat installed and signed in on the machine the
    /// agents actually run on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tooling: Option<serde_json::Value>,
}

/// The outcome of probing git + forge access from the machine that will run
/// them. Push and pull requests are checked separately because they use
/// different credentials — an ssh key and an API login — and one commonly works
/// while the other does not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitCheck {
    /// The forge login in effect (empty when the CLI is not signed in).
    #[serde(default)]
    pub account: String,
    /// The API can see the configured repository — pull requests will work.
    #[serde(default)]
    pub api_ok: bool,
    /// A dry-run push succeeded — the agent can deliver a branch.
    #[serde(default)]
    pub push_ok: bool,
    /// Why a check failed, in the words of the tool that failed it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// A concrete remedy, e.g. an ssh key that GitHub does accept.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub remedy: String,
}

/// Everything a runner advertises about what its machine can do. Grouped so the
/// heartbeat keeps one capability argument as this list grows.
#[derive(Debug, Clone, Default)]
pub struct WorkerCaps {
    pub engines: Vec<String>,
    pub models: Vec<String>,
    pub git: Option<GitCheck>,
    pub tooling: Option<serde_json::Value>,
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

    /// Release a per-ticket stage lease (e.g., after SA/PD/DOCS engine failure).
    /// Default is a no-op (single-runner mode has no coordination).
    async fn release_stage(
        &self,
        _id: &TicketId,
        _stage: &str,
        _worker: &str,
    ) -> Result<(), PortError> {
        Ok(())
    }

    /// Record this runner's live presence (`account@host`, current role, current
    /// ticket, the agent CLIs it can actually run) in the shared worker registry,
    /// so every dashboard can show all teams. Best-effort; the default is a no-op
    /// (single-runner needs none).
    ///
    /// # Errors
    /// [`PortError`] on a coordination-store failure.
    async fn heartbeat_worker(
        &self,
        _worker: &str,
        _role: &str,
        _ticket: &str,
        _caps: &WorkerCaps,
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
