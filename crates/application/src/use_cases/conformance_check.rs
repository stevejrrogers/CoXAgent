//! `RunConformanceUseCase` — the architecture governance step. Scans the
//! codebase against the declared stack rules and files a bug for each drift.
//! Runs deterministically in code (no engine), so drift is caught even when an
//! agent's prose promised to follow the stack and didn't.

use crate::conformance::{self, StackRule};
use crate::error::AppError;
use crate::ports::outbound::StateStorePort;
use crate::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Complexity, Priority, TicketId, TicketType};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Files bugs for architecture-conformance violations.
pub struct RunConformanceUseCase<S: StateStorePort> {
    store: Arc<S>,
    work_dir: PathBuf,
    rules: Vec<StackRule>,
}

impl<S: StateStorePort> RunConformanceUseCase<S> {
    pub fn new(store: Arc<S>, work_dir: PathBuf, rules: Vec<StackRule>) -> Self {
        Self {
            store,
            work_dir,
            rules,
        }
    }

    /// Run the check, filing a bug per new violation. Returns the filed bug ids.
    ///
    /// # Errors
    /// [`AppError`] on load/save failure.
    pub async fn execute(&self) -> Result<Vec<TicketId>, AppError> {
        if self.rules.is_empty() {
            return Ok(Vec::new());
        }
        let violations = conformance::check(&self.work_dir, &self.rules);
        if violations.is_empty() {
            return Ok(Vec::new());
        }

        // Dedupe against existing open bug titles so re-scans don't pile up.
        let existing: HashSet<String> = self
            .store
            .load()
            .await?
            .tickets
            .iter()
            .filter(|t| t.ticket_type() == TicketType::Bug)
            .map(|t| t.title().to_lowercase())
            .collect();

        let adder = AddTicketUseCase::new(Arc::clone(&self.store));
        let mut filed = Vec::new();
        for v in violations {
            let title = v.bug_title();
            if existing.contains(&title.to_lowercase()) {
                continue;
            }
            let id = adder
                .execute(AddTicketInput {
                    ticket_type: TicketType::Bug,
                    title,
                    description: v.message,
                    priority: Priority::High,
                    complexity: Complexity::Medium,
                    has_ui: false,
                })
                .await?;
            filed.push(id);
        }
        Ok(filed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ProjectState;
    use crate::PortError;
    use coxagent_domain::Status;
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

    fn rust_server_rule() -> StackRule {
        StackRule {
            area: "server".to_owned(),
            language: "Rust".to_owned(),
            require_any: vec!["Cargo.toml".to_owned()],
            forbid_ext: vec![".ts".to_owned()],
        }
    }

    #[tokio::test]
    async fn files_bug_for_typescript_server_drift() {
        let dir = tempfile::tempdir().expect("tmp");
        let ts = dir.path().join("server/src/index.ts");
        std::fs::create_dir_all(ts.parent().expect("p")).expect("mkdir");
        std::fs::write(&ts, "export {}").expect("write");

        let store = Arc::new(MemStore::default());
        let uc = RunConformanceUseCase::new(
            Arc::clone(&store),
            dir.path().to_path_buf(),
            vec![rust_server_rule()],
        );
        let filed = uc.execute().await.expect("run");
        assert!(!filed.is_empty(), "should file drift bugs");

        let state = store.load().await.expect("load");
        assert!(state
            .tickets
            .iter()
            .any(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open));

        // Re-run dedupes: no new bugs.
        let again = uc.execute().await.expect("rerun");
        assert!(again.is_empty(), "second scan must not duplicate");
    }

    #[tokio::test]
    async fn no_rules_is_noop() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = Arc::new(MemStore::default());
        let uc = RunConformanceUseCase::new(store, dir.path().to_path_buf(), vec![]);
        assert!(uc.execute().await.expect("run").is_empty());
    }
}
