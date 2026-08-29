//! `JsonStateStore` — file-backed [`StateStorePort`].
//!
//! Guarantees: atomic writes (temp file + rename), an advisory file lock so two
//! processes never write concurrently, validation before persisting, and a
//! rolling backup snapshot on every save so a torn or hand-edited file can be
//! recovered.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{
    QuarantineEntry, StateStorePort, WorkerCaps, WorkerEntry,
};
use coxagent_application::state::{ProjectState, SCHEMA_VERSION};
use coxagent_application::PortError;
use coxagent_domain::{Role, TicketId};
use fs4::fs_std::FileExt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::quarantine::{gate_save, QuarantineLedger};

const STATE_FILE: &str = "state.json";
const LOCK_FILE: &str = ".state.lock";
const BACKUP_DIR: &str = ".backups";
const MAX_BACKUPS: usize = 20;
const COORD_FILE: &str = ".coord.json";
/// Leader lease lifetime — a runner must renew within this or another takes over.
const LEADER_TTL_SECS: i64 = 90;
/// Per-ticket stage lease lifetime — long enough for a slow agent, short enough
/// that a crashed runner's claim frees up for a retry.
const STAGE_TTL_SECS: i64 = 1800;

/// The cross-runner coordination file: who leads, and which per-ticket stages are
/// currently claimed. Kept beside `state.json` and mutated only under the lock.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Coord {
    #[serde(default)]
    leader: Option<Lease>,
    #[serde(default)]
    leases: Vec<StageLease>,
    #[serde(default)]
    workers: Vec<WorkerEntry>,
}

/// How long a worker is shown as online after its last heartbeat.
const WORKER_TTL_SECS: i64 = 600;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Lease {
    worker: String,
    at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StageLease {
    ticket: String,
    stage: String,
    worker: String,
    at: String,
}

/// Age in seconds of an RFC3339 timestamp relative to `now` (also RFC3339);
/// a huge age on parse failure so unparseable leases are treated as expired.
fn age_secs(at: &str, now: &str) -> i64 {
    use time::format_description::well_known::Rfc3339;
    match (
        time::OffsetDateTime::parse(at, &Rfc3339),
        time::OffsetDateTime::parse(now, &Rfc3339),
    ) {
        (Ok(a), Ok(n)) => (n - a).whole_seconds(),
        _ => i64::MAX,
    }
}

/// A [`StateStorePort`] that stores the project aggregate as one JSON file.
pub struct JsonStateStore {
    root: PathBuf,
    /// Audit trail of write-backs the structural-integrity gate refused,
    /// persisted beside the state file (CXA-F229). Shared across the
    /// `spawn_blocking` clones so every writer feeds one ledger.
    quarantine: Arc<QuarantineLedger>,
}

impl JsonStateStore {
    /// Create a store rooted at `dir` (the workspace `state/` directory),
    /// creating it if needed.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the directory cannot be created.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, PortError> {
        let root = dir.into();
        std::fs::create_dir_all(&root).map_err(|e| PortError::Backend(e.to_string()))?;
        Ok(Self {
            quarantine: Arc::new(QuarantineLedger::in_dir(&root)),
            root,
        })
    }

    /// A self-contained copy for `spawn_blocking` (which needs `'static`):
    /// same root, same shared quarantine ledger.
    fn for_blocking(&self) -> Self {
        Self {
            root: self.root.clone(),
            quarantine: Arc::clone(&self.quarantine),
        }
    }

    fn state_path(&self) -> PathBuf {
        self.root.join(STATE_FILE)
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// Blocking load — parse state.json, or default when it is absent. If the
    /// file is present but corrupt, try to recover from the newest good backup.
    fn load_blocking(&self) -> Result<ProjectState, PortError> {
        let path = self.state_path();
        if !path.exists() {
            return Ok(ProjectState::default());
        }
        let bytes = std::fs::read(&path).map_err(|e| PortError::Backend(e.to_string()))?;
        match parse_checked(&bytes) {
            Ok(state) => Ok(state),
            Err(primary) => match self.recover_from_backup() {
                Some(state) => {
                    tracing::warn!("state.json corrupt ({primary}); recovered from backup");
                    Ok(state)
                }
                None => Err(PortError::Corrupt(format!(
                    "state.json corrupt and no usable backup: {primary}"
                ))),
            },
        }
    }

    /// Return the newest backup that parses, if any.
    fn recover_from_backup(&self) -> Option<ProjectState> {
        let dir = self.root.join(BACKUP_DIR);
        let mut backups: Vec<PathBuf> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        backups.sort(); // timestamped names sort oldest -> newest
        backups
            .iter()
            .rev()
            .find_map(|p| std::fs::read(p).ok().and_then(|b| parse_checked(&b).ok()))
    }

    /// Blocking save — lock, then write the state under the lock.
    fn save_blocking(&self, state: &ProjectState) -> Result<(), PortError> {
        let lock = acquire_lock(&self.lock_path())?;
        let result = self.write_locked(state);
        // Lock releases on drop; keep it explicitly alive until here.
        drop(lock);
        result
    }

    /// Validate + audit + atomic-rename + snapshot. The caller must already
    /// hold the exclusive lock (so `claim_blocking` can read-modify-write in
    /// one section). The pre-existing schema-level validation is untouched;
    /// the structural-integrity audit (CXA-F229) is the additional gate that
    /// refuses a corrupted post-state and quarantines its payload.
    fn write_locked(&self, state: &ProjectState) -> Result<(), PortError> {
        // gate_save may HEAL (drop dangling ticket-keyed entries) — work on a
        // clone so the healed shape is what gets persisted.
        let mut state = state.clone();
        let state = {
            gate_save(&mut state, &self.quarantine)?;
            &state
        };

        let json =
            serde_json::to_vec_pretty(state).map_err(|e| PortError::Backend(e.to_string()))?;

        let final_path = self.state_path();
        if final_path.exists() {
            self.snapshot_backup(&final_path)?;
        }
        atomic_write(&self.root, &final_path, &json)
    }

    /// Blocking atomic claim — the whole load/check/set/write runs inside one
    /// held lock, so two processes racing on the same backlog serialize and only
    /// one wins the ticket.
    fn claim_blocking(&self, id: &TicketId, worker: &str, now: &str) -> Result<bool, PortError> {
        let lock = acquire_lock(&self.lock_path())?;
        let outcome = (|| {
            let mut state = self.load_blocking()?;
            let Some(ticket) = state.ticket_mut(id) else {
                return Ok(false);
            };
            if ticket.claimed_by().is_some() {
                return Ok(false);
            }
            if ticket.claim(Role::System, worker, now).is_err() {
                return Ok(false);
            }
            self.write_locked(&state)?;
            Ok(true)
        })();
        drop(lock);
        outcome
    }

    fn coord_path(&self) -> PathBuf {
        self.root.join(COORD_FILE)
    }

    fn read_coord(&self) -> Coord {
        std::fs::read(self.coord_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write_coord(&self, coord: &Coord) -> Result<(), PortError> {
        let json =
            serde_json::to_vec_pretty(coord).map_err(|e| PortError::Backend(e.to_string()))?;
        atomic_write(&self.root, &self.coord_path(), &json)
    }

    /// Acquire or renew leadership under the lock. Wins if there is no leader, the
    /// current leader's lease is stale, or the caller already leads.
    fn acquire_leader_blocking(&self, worker: &str, now: &str) -> Result<bool, PortError> {
        let lock = acquire_lock(&self.lock_path())?;
        let outcome = (|| {
            let mut coord = self.read_coord();
            let can_lead = match &coord.leader {
                None => true,
                Some(l) => l.worker == worker || age_secs(&l.at, now) > LEADER_TTL_SECS,
            };
            if can_lead {
                coord.leader = Some(Lease {
                    worker: worker.to_owned(),
                    at: now.to_owned(),
                });
                self.write_coord(&coord)?;
            }
            Ok(can_lead)
        })();
        drop(lock);
        outcome
    }

    /// Claim a per-ticket stage under the lock, pruning expired leases. Wins if no
    /// live lease exists for `(ticket, stage)` or the caller already holds it.
    fn claim_stage_blocking(
        &self,
        ticket: &str,
        stage: &str,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
        let lock = acquire_lock(&self.lock_path())?;
        let outcome = (|| {
            let mut coord = self.read_coord();
            coord
                .leases
                .retain(|l| age_secs(&l.at, now) <= STAGE_TTL_SECS);
            let held = coord
                .leases
                .iter()
                .find(|l| l.ticket == ticket && l.stage == stage);
            if let Some(l) = held {
                if l.worker != worker {
                    return Ok(false);
                }
            }
            coord
                .leases
                .retain(|l| !(l.ticket == ticket && l.stage == stage));
            coord.leases.push(StageLease {
                ticket: ticket.to_owned(),
                stage: stage.to_owned(),
                worker: worker.to_owned(),
                at: now.to_owned(),
            });
            self.write_coord(&coord)?;
            Ok(true)
        })();
        drop(lock);
        outcome
    }

    /// Upsert this worker's presence under the lock, pruning stale entries.
    fn heartbeat_blocking(
        &self,
        worker: &str,
        role: &str,
        ticket: &str,
        caps: &WorkerCaps,
        now: &str,
    ) -> Result<(), PortError> {
        let lock = acquire_lock(&self.lock_path())?;
        let mut coord = self.read_coord();
        coord
            .workers
            .retain(|w| w.worker != worker && age_secs(&w.at, now) <= WORKER_TTL_SECS);
        coord.workers.push(WorkerEntry {
            worker: worker.to_owned(),
            role: role.to_owned(),
            ticket: ticket.to_owned(),
            at: now.to_owned(),
            engines: caps.engines.clone(),
            models: caps.models.clone(),
            git: caps.git.clone(),
            tooling: caps.tooling.clone(),
            version: caps.version.clone(),
        });
        let outcome = self.write_coord(&coord);
        drop(lock);
        outcome
    }

    /// Live worker registry (stale entries dropped) relative to `now`.
    fn workers_blocking(&self, now: &str) -> Vec<WorkerEntry> {
        self.read_coord()
            .workers
            .into_iter()
            .filter(|w| age_secs(&w.at, now) <= WORKER_TTL_SECS)
            .collect()
    }

    /// Copy the current state file into a timestamped, pruned backup set.
    fn snapshot_backup(&self, current: &Path) -> Result<(), PortError> {
        let dir = self.root.join(BACKUP_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| PortError::Backend(e.to_string()))?;
        let stamp = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
        let dest = dir.join(format!("state-{stamp}.json"));
        std::fs::copy(current, &dest).map_err(|e| PortError::Backend(e.to_string()))?;
        prune_backups(&dir, MAX_BACKUPS);
        Ok(())
    }
}

#[async_trait]
impl StateStorePort for JsonStateStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        let store = self.for_blocking();
        tokio::task::spawn_blocking(move || store.load_blocking())
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        let store = self.for_blocking();
        let state = state.clone();
        tokio::task::spawn_blocking(move || store.save_blocking(&state))
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?
    }

    async fn claim_ticket(
        &self,
        id: &TicketId,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
        let store = self.for_blocking();
        let id = id.clone();
        let worker = worker.to_owned();
        let now = now.to_owned();
        tokio::task::spawn_blocking(move || store.claim_blocking(&id, &worker, &now))
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?
    }

    async fn acquire_leader(&self, worker: &str, now: &str) -> Result<bool, PortError> {
        let store = self.for_blocking();
        let worker = worker.to_owned();
        let now = now.to_owned();
        tokio::task::spawn_blocking(move || store.acquire_leader_blocking(&worker, &now))
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?
    }

    async fn claim_stage(
        &self,
        id: &TicketId,
        stage: &str,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
        let store = self.for_blocking();
        let (ticket, stage, worker, now) = (
            id.to_string(),
            stage.to_owned(),
            worker.to_owned(),
            now.to_owned(),
        );
        tokio::task::spawn_blocking(move || {
            store.claim_stage_blocking(&ticket, &stage, &worker, &now)
        })
        .await
        .map_err(|e| PortError::Backend(e.to_string()))?
    }

    async fn heartbeat_worker(
        &self,
        worker: &str,
        role: &str,
        ticket: &str,
        caps: &WorkerCaps,
        now: &str,
    ) -> Result<(), PortError> {
        let store = self.for_blocking();
        let (worker, role, ticket, now) = (
            worker.to_owned(),
            role.to_owned(),
            ticket.to_owned(),
            now.to_owned(),
        );
        let caps = caps.clone();
        tokio::task::spawn_blocking(move || {
            store.heartbeat_blocking(&worker, &role, &ticket, &caps, &now)
        })
        .await
        .map_err(|e| PortError::Backend(e.to_string()))?
    }

    async fn workers(&self) -> Result<Vec<WorkerEntry>, PortError> {
        let store = self.for_blocking();
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        tokio::task::spawn_blocking(move || Ok(store.workers_blocking(&now)))
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?
    }

    async fn quarantined(&self) -> Vec<QuarantineEntry> {
        self.quarantine.recent()
    }
}

/// Parse bytes into state and reject a schema newer than we understand.
fn parse_checked(bytes: &[u8]) -> Result<ProjectState, PortError> {
    let state: ProjectState =
        serde_json::from_slice(bytes).map_err(|e| PortError::Corrupt(e.to_string()))?;
    if state.schema_version > SCHEMA_VERSION {
        return Err(PortError::Corrupt(format!(
            "state schema_version {} is newer than supported {SCHEMA_VERSION}; upgrade coxagent",
            state.schema_version
        )));
    }
    Ok(state)
}

/// Acquire an exclusive advisory lock on the lock file.
pub(crate) fn acquire_lock(path: &Path) -> Result<File, PortError> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| PortError::Backend(e.to_string()))?;
    FileExt::lock_exclusive(&file).map_err(|e| PortError::Backend(e.to_string()))?;
    Ok(file)
}

/// Write bytes to a temp file in the same directory, fsync, then rename over the
/// destination. Rename within a directory is atomic on POSIX and Windows.
pub(crate) fn atomic_write(dir: &Path, final_path: &Path, bytes: &[u8]) -> Result<(), PortError> {
    let tmp = dir.join(format!(".state.tmp.{}", std::process::id()));
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| PortError::Backend(e.to_string()))?;
        f.write_all(bytes)
            .map_err(|e| PortError::Backend(e.to_string()))?;
        f.sync_all()
            .map_err(|e| PortError::Backend(e.to_string()))?;
    }
    std::fs::rename(&tmp, final_path).map_err(|e| PortError::Backend(e.to_string()))?;
    Ok(())
}

/// Keep only the newest `keep` backups by filename (timestamps sort lexically).
fn prune_backups(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    if files.len() > keep {
        for old in &files[..files.len() - keep] {
            let _ = std::fs::remove_file(old);
        }
    }
}

#[cfg(test)]
mod coord_tests {
    use super::*;

    fn store() -> JsonStateStore {
        // `keep` prevents the tempdir from auto-deleting on drop for the test.
        let dir = tempfile::tempdir().expect("tempdir").keep();
        JsonStateStore::new(dir).expect("store")
    }

    #[tokio::test]
    async fn only_one_worker_leads_at_a_time() {
        let s = store();
        let now = "2026-07-15T00:00:00Z";
        assert!(s.acquire_leader("chopper@mac", now).await.unwrap());
        // A different worker cannot lead while the lease is fresh.
        assert!(!s.acquire_leader("luffy@mac", now).await.unwrap());
        // The holder renews freely.
        assert!(s.acquire_leader("chopper@mac", now).await.unwrap());
        // After the lease goes stale, another worker takes over.
        let later = "2026-07-15T00:05:00Z"; // 300s > LEADER_TTL_SECS
        assert!(s.acquire_leader("luffy@mac", later).await.unwrap());
    }

    #[tokio::test]
    async fn stage_claim_is_exclusive_per_ticket() {
        let s = store();
        let id = TicketId::new("CXC-F001").expect("id");
        let now = "2026-07-15T00:00:00Z";
        assert!(s.claim_stage(&id, "sa", "chopper@mac", now).await.unwrap());
        // Another worker cannot take the same stage on the same ticket.
        assert!(!s.claim_stage(&id, "sa", "luffy@mac", now).await.unwrap());
        // A different stage on the same ticket is independent.
        assert!(s.claim_stage(&id, "pd", "luffy@mac", now).await.unwrap());
        // A different ticket's same stage is independent.
        let id2 = TicketId::new("CXC-F002").expect("id");
        assert!(s.claim_stage(&id2, "sa", "luffy@mac", now).await.unwrap());
    }
}
