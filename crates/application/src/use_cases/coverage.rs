//! `RunCoverageUseCase` — the coverage-gap detection step (CXA-F007), run after
//! DEV ships code so the team learns where its tests are thin.
//!
//! The TEST role files bugs but never notices UNTESTED code, and the BA has no
//! structured data to propose test-improvement tickets. This is that missing
//! step: a static, deterministic scan (no compile, no coverage-flag build) that
//! finds top-level production functions with no test reaching them, groups them
//! by module, and — for modules over the configured threshold — files a
//! low-priority CHORE ticket, deduped so re-runs don't pile up. The heavy
//! reasoning lives in [`crate::codegraph::CoverageGapAnalysis`] (a pure function
//! of the code graph + source), exactly as the SA's design specifies; this use
//! case is the thin adapter that gathers the tree through the files port and
//! writes state.

use crate::codegraph::{BAChoreTicketProposal, CodeGraph, CoverageGapAnalysis};
use crate::config::Config;
use crate::error::AppError;
use crate::ports::outbound::StateStorePort;
use crate::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Complexity, Priority, TicketId, TicketType};
use std::path::PathBuf;
use std::sync::Arc;

/// Runs the coverage-gap scan and files chore tickets for the worst modules.
pub struct RunCoverageUseCase<S: StateStorePort> {
    store: Arc<S>,
    work_dir: PathBuf,
    config: Config,
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
}

impl<S: StateStorePort> RunCoverageUseCase<S> {
    pub fn new(store: Arc<S>, work_dir: PathBuf, config: Config) -> Self {
        Self {
            store,
            work_dir,
            config,
            files: None,
        }
    }

    /// Attach the files port the scan reads through. Without it (tests) the
    /// step is a no-op — there is nothing to scan.
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    /// Run the scan, filing a low-priority chore per module whose uncovered
    /// functions clear the configured threshold. Returns the ids filed.
    ///
    /// # Errors
    /// [`AppError`] on load/save failure.
    pub async fn execute(&self) -> Result<Vec<TicketId>, AppError> {
        // Opt-out switch: a project that doesn't want coverage chores turns the
        // gate off rather than editing every proposal out of its backlog.
        if !self.config.coverage.enabled {
            return Ok(Vec::new());
        }
        let Some(files) = &self.files else {
            return Ok(Vec::new());
        };

        // Reuse the index when the cycle already built it; otherwise build one.
        // `scan` re-reads source through the port regardless (the cfg/test
        // region scans need the text the index does not carry).
        let graph = match CodeGraph::load(files.as_ref(), &self.work_dir).await {
            Some(g) => g,
            None => CodeGraph::index(files.as_ref(), &self.work_dir).await,
        };
        let analysis = CoverageGapAnalysis::scan(
            &graph,
            files.as_ref(),
            &self.work_dir,
            &self.work_dir,
        )
        .await;
        let proposal =
            BAChoreTicketProposal::with_threshold(&analysis, self.config.coverage.threshold());
        if proposal.tickets.is_empty() {
            return Ok(Vec::new());
        }

        // Dedupe against existing chores so re-scans don't stack duplicate
        // tickets for the same module.
        let existing: std::collections::HashSet<String> = self
            .store
            .load()
            .await?
            .tickets
            .iter()
            .filter(|t| t.ticket_type() == TicketType::Chore)
            .map(|t| t.title().to_lowercase())
            .collect();

        let adder = AddTicketUseCase::new(Arc::clone(&self.store));
        let mut created_ids = Vec::new();
        for ticket in proposal.tickets {
            if existing.contains(&ticket.title.to_lowercase()) {
                continue;
            }
            let id = adder
                .execute(AddTicketInput {
                    ticket_type: TicketType::Chore,
                    title: ticket.title,
                    description: ticket.description,
                    priority: Priority::Low,
                    complexity: Complexity::Small,
                    has_ui: false,
                    acceptance_criteria: Vec::new(),
                })
                .await?;
            created_ids.push(id);
        }
        Ok(created_ids)
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

    /// Write a tiny rust project with one module that has 4 untested top-level
    /// functions (over the default >3 threshold), mirroring the codegraph
    /// coverage fixture but self-contained.
    fn fixture(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("src")).expect("mk");
        std::fs::write(
            root.join("src/app.rs"),
            "pub fn one() {}\npub fn two() {}\npub fn three() {}\npub fn four() {}\n",
        )
        .expect("write");
    }

    fn uc(
        store: Arc<MemStore>,
        config: Config,
        root: &std::path::Path,
    ) -> RunCoverageUseCase<MemStore> {
        RunCoverageUseCase::new(store, root.to_path_buf(), config)
            .with_files(Some(Arc::new(crate::test_fs::StdFsFiles)))
    }

    #[tokio::test]
    async fn files_a_low_priority_chore_when_a_module_clears_the_threshold() {
        let dir = tempfile::tempdir().expect("tmp");
        fixture(dir.path());
        let store = Arc::new(MemStore::default());

        let filed = uc(Arc::clone(&store), Config::default(), dir.path())
            .execute()
            .await
            .expect("run");

        assert_eq!(filed.len(), 1, "one module above threshold → one chore");
        let state = store.load().await.expect("load");
        let t = state.tickets[0].clone();
        assert_eq!(t.ticket_type(), TicketType::Chore);
        assert_eq!(t.priority(), Priority::Low);
        assert_eq!(t.status(), Status::Pending);
        assert!(
            t.title().contains("src"),
            "title names the module: {}",
            t.title()
        );
    }

    #[tokio::test]
    async fn re_runs_do_not_duplicate_chores() {
        let dir = tempfile::tempdir().expect("tmp");
        fixture(dir.path());
        let store = Arc::new(MemStore::default());

        let uc = uc(Arc::clone(&store), Config::default(), dir.path());
        let first = uc.execute().await.expect("first");
        assert_eq!(first.len(), 1);
        let second = uc.execute().await.expect("second");
        assert!(
            second.is_empty(),
            "a second scan must not file a duplicate chore"
        );
        assert_eq!(store.load().await.expect("load").tickets.len(), 1);
    }

    #[tokio::test]
    async fn a_module_under_the_threshold_files_nothing() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(dir.path().join("src")).expect("mk");
        // Only 2 uncovered functions — below the default >3 threshold.
        std::fs::write(
            dir.path().join("src/app.rs"),
            "pub fn one() {}\npub fn two() {}\n",
        )
        .expect("write");
        let store = Arc::new(MemStore::default());
        let filed = uc(Arc::clone(&store), Config::default(), dir.path())
            .execute()
            .await
            .expect("run");
        assert!(filed.is_empty());
    }

    #[tokio::test]
    async fn disabled_coverage_config_is_a_noop() {
        let dir = tempfile::tempdir().expect("tmp");
        fixture(dir.path());
        let store = Arc::new(MemStore::default());
        let mut config = Config::default();
        config.coverage.enabled = false;
        let filed = uc(Arc::clone(&store), config, dir.path())
            .execute()
            .await
            .expect("run");
        assert!(filed.is_empty());
        assert!(store.load().await.expect("load").tickets.is_empty());
    }

    #[tokio::test]
    async fn configurable_threshold_is_honoured() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(dir.path().join("src")).expect("mk");
        std::fs::write(
            dir.path().join("src/app.rs"),
            "pub fn one() {}\npub fn two() {}\npub fn three() {}\n",
        )
        .expect("write");
        let store = Arc::new(MemStore::default());
        // Default threshold 3 is NOT cleared by exactly 3 uncovered (needs >3),
        // so nothing files...
        let filed = uc(Arc::clone(&store), Config::default(), dir.path())
            .execute()
            .await
            .expect("run");
        assert!(filed.is_empty());

        // ...but a threshold of 2 clears it.
        let store2 = Arc::new(MemStore::default());
        let mut config = Config::default();
        config.coverage.threshold = 2;
        let filed2 = uc(Arc::clone(&store2), config, dir.path())
            .execute()
            .await
            .expect("run");
        assert_eq!(filed2.len(), 1);
    }
}
