//! `RunConformanceUseCase` — the architecture governance step. Scans the
//! codebase against the declared stack rules and files a bug for each drift.
//! Runs deterministically in code (no engine), so drift is caught even when an
//! agent's prose promised to follow the stack and didn't.

use crate::conformance::{self, StackRule};
use crate::error::AppError;
use crate::ports::outbound::StateStorePort;
use crate::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Complexity, Priority, TicketId, TicketType};
use std::path::PathBuf;
use std::sync::Arc;

/// Files bugs for architecture-conformance violations.
pub struct RunConformanceUseCase<S: StateStorePort> {
    store: Arc<S>,
    work_dir: PathBuf,
    rules: Vec<StackRule>,
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
}

impl<S: StateStorePort> RunConformanceUseCase<S> {
    pub fn new(store: Arc<S>, work_dir: PathBuf, rules: Vec<StackRule>) -> Self {
        Self {
            store,
            work_dir,
            rules,
            files: None,
        }
    }

    /// Attach the files port the scan reads through. Without it the check is
    /// a no-op — there is nothing to scan.
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    /// Run the check, filing a bug per new violation and reconciling the
    /// operator-facing drift-alert surface (`ProjectState::drift_alerts`)
    /// against this scan: alerts open with their bug's id, survive re-scans
    /// keyed by (area, message), and clear automatically once resolved. The
    /// cleared surface is SAVED even when nothing was filed — a resolving
    /// scan must reach storage and the dashboard without a manual dismissal.
    /// Returns the ids of the bugs this pass filed.
    ///
    /// # Errors
    /// [`AppError`] on load/save failure.
    pub async fn execute(&self) -> Result<Vec<TicketId>, AppError> {
        if self.rules.is_empty() {
            return Ok(Vec::new());
        }
        let Some(workspace) = &self.files else {
            return Ok(Vec::new());
        };
        let mut by_area = std::collections::BTreeMap::new();
        for rule in &self.rules {
            let listed: Vec<String> = workspace
                .list_recursive(&self.work_dir.join(&rule.area))
                .await
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            by_area.insert(rule.area.clone(), listed);
        }
        let violations = conformance::check(&by_area, &self.rules);

        // Clean scan: the only surface work possible is clearing, and there
        // is nothing to clear unless a previous scan left alerts open.
        if violations.is_empty() {
            let mut state = self.store.load().await?;
            if state.drift_alerts.is_empty() {
                return Ok(Vec::new());
            }
            state.sync_drift_alerts(&[], &|_| None);
            self.store.save(&state).await?;
            return Ok(Vec::new());
        }

        // Bug inventory by title (the filing dedupe key): at most one bug per
        // drift title exists — filing refuses duplicates — so the scan's alert
        // links resolve through this map. Scoped so the stale aggregate is
        // visibly dead before filing starts: filing saves through the adder.
        let mut bug_by_title: std::collections::BTreeMap<String, String> = {
            let state = self.store.load().await?;
            state
                .tickets
                .iter()
                .filter(|t| t.ticket_type() == TicketType::Bug)
                .map(|t| (t.title().to_lowercase(), t.id().as_str().to_owned()))
                .collect()
        };

        let adder = AddTicketUseCase::new(Arc::clone(&self.store));
        let mut filed = Vec::new();
        for v in &violations {
            let title = v.bug_title();
            let key = title.to_lowercase();
            if bug_by_title.contains_key(&key) {
                continue;
            }
            let outcome = adder
                .execute(AddTicketInput {
                    ticket_type: TicketType::Bug,
                    title,
                    description: v.message.clone(),
                    priority: Priority::High,
                    complexity: Complexity::Medium,
                    has_ui: false,
                    acceptance_criteria: Vec::new(),
                    goal: None,
                    service_tag: None,
                })
                .await;
            match outcome {
                Ok(id) => {
                    bug_by_title.insert(key, id.as_str().to_owned());
                    filed.push(id);
                }
                // Two drifts can share a theme in one sweep; the gate refusing
                // the second is correct — skip it, never abort the sweep.
                Err(e)
                    if e.to_string()
                        .contains(crate::use_cases::add_ticket::DUPLICATE_REFUSED) => {}
                Err(e) => return Err(e),
            }
        }

        // Reconcile the alert surface on a FRESH load: filing saved the new
        // bugs into state, so the stale aggregate above must not be saved back.
        let mut state = self.store.load().await?;
        if state.sync_drift_alerts(&violations, &|v| {
            bug_by_title.get(&v.bug_title().to_lowercase()).cloned()
        }) {
            self.store.save(&state).await?;
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

    /// In-memory workspace: `list_recursive` answers from a fixed path list.
    struct FixedFiles(Vec<std::path::PathBuf>);
    #[async_trait::async_trait]
    impl crate::ports::outbound::WorkspaceFilesPort for FixedFiles {
        async fn read(&self, _: &std::path::Path) -> Option<String> {
            None
        }
        async fn write(&self, _: &std::path::Path, _: &str) -> bool {
            false
        }
        async fn write_bytes(&self, _: &std::path::Path, _: &[u8]) -> bool {
            false
        }
        async fn delete(&self, _: &std::path::Path) -> bool {
            false
        }
        async fn stat(&self, _: &std::path::Path) -> Option<crate::ports::outbound::FileMeta> {
            None
        }
        async fn list_recursive(&self, dir: &std::path::Path) -> Vec<std::path::PathBuf> {
            self.0
                .iter()
                .filter(|p| p.starts_with(dir))
                .cloned()
                .collect()
        }
        async fn list_dirs(&self, _: &std::path::Path) -> Vec<std::path::PathBuf> {
            Vec::new()
        }
        async fn list(&self, _: &std::path::Path) -> Vec<crate::ports::outbound::FileMeta> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn files_bug_for_typescript_server_drift() {
        let root = std::path::PathBuf::from("/w");
        let store = Arc::new(MemStore::default());
        let uc =
            RunConformanceUseCase::new(Arc::clone(&store), root.clone(), vec![rust_server_rule()])
                .with_files(Some(Arc::new(FixedFiles(vec![
                    root.join("server/src/index.ts")
                ]))));
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

    /// Regression (CXA-F226): the scan reconciles the drift-alert surface —
    /// alerts open linked to the filed bug, and a resolving scan clears them
    /// from storage even though nothing was filed.
    #[tokio::test]
    async fn scan_opens_alerts_linked_to_their_bug_and_a_resolving_scan_clears_them() {
        let root = std::path::PathBuf::from("/w");
        let store = Arc::new(MemStore::default());
        let dirty =
            RunConformanceUseCase::new(Arc::clone(&store), root.clone(), vec![rust_server_rule()])
                .with_files(Some(Arc::new(FixedFiles(vec![
                    root.join("server/src/index.ts")
                ]))));
        let filed = dirty.execute().await.expect("run");
        assert_eq!(
            filed.len(),
            1,
            "both violations of one area share one bug title"
        );

        let state = store.load().await.expect("load");
        assert_eq!(state.drift_alerts.len(), 2, "one alert per (area, message)");
        assert!(
            state
                .drift_alerts
                .iter()
                .all(|a| a.area == "server" && a.ticket == filed[0].as_str()),
            "every alert names the area and links the filed bug: {:?}",
            state.drift_alerts
        );

        let clean =
            RunConformanceUseCase::new(Arc::clone(&store), root.clone(), vec![rust_server_rule()])
                .with_files(Some(Arc::new(FixedFiles(vec![
                    root.join("server/Cargo.toml"),
                    root.join("server/src/main.rs"),
                ]))));
        assert!(clean.execute().await.expect("run").is_empty());
        let state = store.load().await.expect("load");
        assert!(
            state.drift_alerts.is_empty(),
            "a resolving scan clears the surface from storage: {:?}",
            state.drift_alerts
        );
    }
}
