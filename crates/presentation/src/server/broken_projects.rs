// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Projects the hub could not load, kept visible instead of dropped.
//!
//! A `coxagent.json` that does not parse now fails the project's load rather
//! than answering with `Config::default()`, whose empty `policy` would turn the
//! governance gates off unasked (COX-B043). Failing closed is only half the
//! answer: a project that fails to build never entered the hub's project map,
//! so `/api/projects` never mentioned it and the dashboard showed nothing at
//! all — the only trace was one line in the hub log, which is exactly where a
//! person who cannot find their project will not look.
//!
//! The record here travels beside the healthy handles so the listing can say
//! "this project is registered, and here is the field that stops it running".
//!
//! Since CXA-B114 the label is also no longer forever: the composition root
//! retries failed loads in the background and ships a recovered handle to
//! [`admit_recovered_projects`], which swaps the broken entry for a live one
//! without a restart.

use super::*;

/// A registered project the hub could not load, and why.
///
/// Deliberately NOT a [`ProjectHandle`] variant: a handle owns a store, a
/// runner and an engine, and every route behind it assumes those exist. A
/// broken project has none of them — it is a label and a reason, and the type
/// says so.
#[derive(Clone, Debug)]
pub struct BrokenProject {
    /// Registry id — the same id the healthy handle would have carried.
    pub id: String,
    /// The file a person has to fix, so the message is actionable without
    /// knowing the hub's directory layout.
    pub config_path: PathBuf,
    /// Why the load failed, already naming the offending field where the config
    /// parser could (e.g. `deploy.host_port: invalid value ...`).
    pub error: String,
}

/// The `/api/projects` entries for projects that failed to load.
///
/// Flagged `broken` so a client can list them but refuse to SELECT one: there
/// is no store, no runner and no route behind a broken project, so switching to
/// it would only produce 404s. `name` falls back to the id because the display
/// name lives in a state file this project never got to read.
pub(super) fn broken_entries(broken: &[BrokenProject]) -> Vec<serde_json::Value> {
    broken
        .iter()
        .map(|b| {
            serde_json::json!({
                "id": b.id,
                "name": b.id,
                "alias": "",
                "broken": true,
                "error": b.error,
                "config_path": b.config_path.display().to_string(),
            })
        })
        .collect()
}

/// Admit projects the composition root recovered after boot (CXA-B114): each
/// handle joins the live registry (map + order) and its broken entry — the
/// label that said why it could not load — is cleared, so `/api/projects` and
/// the fleet river reflect the recovery without a restart. A duplicate id
/// (the project somehow already live) is refused rather than double-registered
/// or allowed to mask a still-broken state.
pub(super) async fn admit_recovered_projects(
    app: AppState,
    mut recoveries: tokio::sync::mpsc::Receiver<ProjectHandle>,
) {
    while let Some(p) = recoveries.recv().await {
        let id = p.id.clone();
        {
            let mut projects = app.projects.write().await;
            if projects.contains_key(&id) {
                tracing::warn!("recovered project '{id}' is already live; refusing a duplicate");
                continue;
            }
            projects.insert(id.clone(), p);
        }
        app.order.write().await.push(id.clone());
        let mut broken = app.broken.write().await;
        let stale = broken.iter().filter(|b| b.id == id).count();
        broken.retain(|b| b.id != id);
        drop(broken);
        tracing::info!(
            "project '{id}' recovered after boot — registered live, {stale} broken label(s) cleared"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::status::build_state;
    use super::super::store_rpc_test_support::UnusedEngine;
    use super::{
        admit_recovered_projects, broken_entries, AppState, BrokenProject, HubExtras, ProjectHandle,
    };
    use coxagent_application::use_cases::RunnerHandle;

    fn broken(id: &str, error: &str) -> BrokenProject {
        BrokenProject {
            id: id.to_owned(),
            config_path: std::path::PathBuf::from("/w")
                .join(id)
                .join("coxagent.json"),
            error: error.to_owned(),
        }
    }

    /// The point of the ticket's UI half: the reason a project is missing has
    /// to reach the person looking for it, not just the log.
    #[test]
    fn a_broken_project_carries_its_reason_and_the_file_to_fix() {
        let entries = broken_entries(&[broken(
            "cox",
            "invalid /w/cox/coxagent.json: deploy.host_port: invalid value",
        )]);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], "cox");
        assert_eq!(entries[0]["broken"], true);
        assert_eq!(
            entries[0]["error"],
            "invalid /w/cox/coxagent.json: deploy.host_port: invalid value"
        );
        assert_eq!(entries[0]["config_path"], "/w/cox/coxagent.json");
    }

    /// A broken entry must be labelled even though its display name lives in a
    /// state file the hub never got to read — an unnamed row is a row nobody
    /// can act on.
    #[test]
    fn a_broken_project_is_labelled_by_its_id_when_it_has_no_name_yet() {
        let entries = broken_entries(&[broken("cox", "unreadable")]);

        assert_eq!(entries[0]["name"], "cox");
    }

    /// The healthy hub: nothing extra appended, so the listing is unchanged for
    /// every project that loads.
    #[test]
    fn a_hub_with_nothing_broken_appends_nothing() {
        assert!(broken_entries(&[]).is_empty());
    }

    // ---- CXA-B114: recovery admission ------------------------------------

    /// A live-looking handle like the ones the composition root sends after a
    /// successful background rebuild.
    fn stub_handle(id: &str) -> ProjectHandle {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        ProjectHandle {
            id: id.to_owned(),
            name: id.to_owned(),
            alias: String::new(),
            store: std::sync::Arc::new(
                coxagent_infrastructure::JsonStateStore::new(&dir).expect("store"),
            ),
            runner: std::sync::Arc::new(RunnerHandle::default()),
            config_path: dir.join("coxagent.json"),
            engine: std::sync::Arc::new(UnusedEngine),
            work_dir: dir.clone(),
            budget: std::sync::Arc::new(std::sync::Mutex::new(
                coxagent_application::BudgetCaps::default(),
            )),
            context_path: dir.join("project_context.md"),
            forge: None,
            deploy: None,
            outbox: None,
            storage: None,
            files: None,
            deps_discovery: None,
        }
    }

    /// An AppState carrying one broken registration, ready for an admit task.
    async fn hub_with_broken(id: &str) -> AppState {
        let hub_dir = tempfile::tempdir().expect("tempdir");
        build_state(
            Vec::new(),
            std::sync::Arc::new(coxagent_infrastructure::MemoryAuditSink::default()),
            HubExtras {
                hub_dir: Some(hub_dir.path().to_path_buf()),
                broken: vec![broken(id, "backend failure: connection: db error")],
                ..Default::default()
            },
        )
        .await
    }

    /// The moment the recovered handle lands it must be served like any
    /// project that loaded at boot — and its broken label must be gone.
    #[tokio::test]
    async fn a_recovered_project_joins_the_registry_and_clears_its_broken_label() {
        let state = hub_with_broken("late").await;
        let (admit, rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(admit_recovered_projects(state.clone(), rx));

        admit.send(stub_handle("late")).await.expect("admit");
        for _ in 0..100 {
            if state.project("late").await.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert!(
            state.project("late").await.is_some(),
            "the recovered handle must be served by every project route"
        );
        assert!(
            state.broken.read().await.is_empty(),
            "the stale broken label must not outlive the recovery"
        );
        assert_eq!(
            state.order.read().await.last().map(String::as_str),
            Some("late"),
            "the recovered project joins the registration order"
        );
    }

    /// An id that is somehow already live must not be double-registered — and
    /// a still-present broken label for it must not be silently cleared by a
    /// duplicate admission. A sentinel message after the duplicate proves the
    /// duplicate was actually processed (FIFO) before the assertions run.
    #[tokio::test]
    async fn a_recovered_duplicate_is_refused_rather_than_double_registered() {
        let hub_dir = tempfile::tempdir().expect("tempdir");
        let state = build_state(
            vec![stub_handle("cxa")],
            std::sync::Arc::new(coxagent_infrastructure::MemoryAuditSink::default()),
            HubExtras {
                hub_dir: Some(hub_dir.path().to_path_buf()),
                broken: vec![broken("cxa", "stale label")],
                ..Default::default()
            },
        )
        .await;
        let (admit, rx) = tokio::sync::mpsc::channel(2);
        tokio::spawn(admit_recovered_projects(state.clone(), rx));

        let mut duplicate = stub_handle("cxa");
        duplicate.name = "duplicate".to_owned();
        admit.send(duplicate).await.expect("admit duplicate");
        admit
            .send(stub_handle("sentinel"))
            .await
            .expect("admit sentinel");
        for _ in 0..100 {
            if state.project("sentinel").await.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            state.project("sentinel").await.is_some(),
            "the sentinel must be admitted for the ordering argument to hold"
        );

        let projects = state.projects.read().await;
        assert_eq!(projects.len(), 2, "no second registration of 'cxa'");
        assert_eq!(
            projects.get("cxa").map(|p| p.name.as_str()),
            Some("cxa"),
            "the original handle is kept, not replaced by the duplicate"
        );
        drop(projects);
        assert_eq!(
            state.broken.read().await.len(),
            1,
            "the refused duplicate must not clear anything"
        );
        assert_eq!(state.order.read().await.len(), 2);
    }
}
