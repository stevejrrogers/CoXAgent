//! `JsonStateStore` — file-backed [`StateStorePort`].
//!
//! Guarantees: atomic writes (temp file + rename), an advisory file lock so two
//! processes never write concurrently, validation before persisting, and a
//! rolling backup snapshot on every save so a torn or hand-edited file can be
//! recovered.

use async_trait::async_trait;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::{ProjectState, SCHEMA_VERSION};
use coxagent_application::PortError;
use fs4::fs_std::FileExt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const STATE_FILE: &str = "state.json";
const LOCK_FILE: &str = ".state.lock";
const BACKUP_DIR: &str = ".backups";
const MAX_BACKUPS: usize = 20;

/// A [`StateStorePort`] that stores the project aggregate as one JSON file.
pub struct JsonStateStore {
    root: PathBuf,
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
        Ok(Self { root })
    }

    fn state_path(&self) -> PathBuf {
        self.root.join(STATE_FILE)
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// Blocking load — parse state.json, or default when it is absent.
    fn load_blocking(&self) -> Result<ProjectState, PortError> {
        let path = self.state_path();
        if !path.exists() {
            return Ok(ProjectState::default());
        }
        let bytes = std::fs::read(&path).map_err(|e| PortError::Backend(e.to_string()))?;
        let state: ProjectState =
            serde_json::from_slice(&bytes).map_err(|e| PortError::Corrupt(e.to_string()))?;
        if state.schema_version > SCHEMA_VERSION {
            return Err(PortError::Corrupt(format!(
                "state schema_version {} is newer than supported {SCHEMA_VERSION}; upgrade coxagent",
                state.schema_version
            )));
        }
        Ok(state)
    }

    /// Blocking save — lock, validate, atomic rename, snapshot backup.
    fn save_blocking(&self, state: &ProjectState) -> Result<(), PortError> {
        state
            .validate()
            .map_err(|e| PortError::Corrupt(format!("refusing to save invalid state: {e}")))?;

        let lock = acquire_lock(&self.lock_path())?;

        let json =
            serde_json::to_vec_pretty(state).map_err(|e| PortError::Backend(e.to_string()))?;

        let final_path = self.state_path();
        if final_path.exists() {
            self.snapshot_backup(&final_path)?;
        }
        atomic_write(&self.root, &final_path, &json)?;

        // Lock releases on drop; keep it explicitly alive until here.
        drop(lock);
        Ok(())
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
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || JsonStateStore { root }.load_blocking())
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        let root = self.root.clone();
        let state = state.clone();
        tokio::task::spawn_blocking(move || JsonStateStore { root }.save_blocking(&state))
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?
    }
}

/// Acquire an exclusive advisory lock on the lock file.
fn acquire_lock(path: &Path) -> Result<File, PortError> {
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
fn atomic_write(dir: &Path, final_path: &Path, bytes: &[u8]) -> Result<(), PortError> {
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
