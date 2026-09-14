// CXA-B130 tests — DELETE /api/projects/:pid must purge the project's
// persisted state, not just deregister it.
//
// Pure in-process verification: requests drive the real handler behind the
// real `auth_mw` layering from `serve_full`, answered via
// `tower::ServiceExt::oneshot`. No hub process, no TCP port.
//
// The bug this locks down: the endpoint used to remove only the registry
// entry, in-memory handle and (best-effort) disk dirs, while the project's
// row in the shared `project_state` store survived. Recreating a project
// whose derived id collided silently adopted the stale state (deleted
// tickets/spend reappearing, and the onboarding "workspace already has
// tickets" refusal answering 500 instead of a fresh workspace).
//
// Encoded acceptance criteria:
// - AC1  a successful DELETE purges the stored state exactly once, BEFORE the
//        registry deregistration, removes the workspace scaffolding from disk
//        (awaited, not fire-and-forget), and deregisters the project.
// - AC2  a FAILED purge never answers 200 and aborts before the registry
//        removal and the disk cleanup — a 200 that leaves the stored row
//        behind is exactly the resurrection bug.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]
use super::*;
use axum::body::Body;
use coxagent_application::config::BudgetCaps;
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;
use coxagent_infrastructure::MemoryAuditSink;
use std::sync::Mutex;
use tower::ServiceExt;

/// The project under delete in these tests (harness convention).
const PID: &str = "demo";

/// Events in the exact order the delete flow performed them: "purge" when the
/// store's delete ran, "deregister" when the registry remover was invoked.
type EventLog = Arc<Mutex<Vec<&'static str>>>;

/// A store that records whether the delete flow actually purged it, and can
/// simulate the store being unreachable (the 5xx path). Everything else is
/// the port's single-runner default — irrelevant to this flow.
struct RecordingStore {
    events: EventLog,
    fail_delete: bool,
}

impl RecordingStore {
    fn new(events: EventLog, fail_delete: bool) -> Arc<Self> {
        Arc::new(Self {
            events,
            fail_delete,
        })
    }
}

#[async_trait::async_trait]
impl StateStorePort for RecordingStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(ProjectState::default())
    }

    async fn save(&self, _state: &ProjectState) -> Result<(), PortError> {
        Ok(())
    }

    async fn delete(&self) -> Result<(), PortError> {
        self.events.lock().expect("event log").push("purge");
        if self.fail_delete {
            Err(PortError::Backend("store unreachable".into()))
        } else {
            Ok(())
        }
    }
}

/// One registered project in a hub whose `remover` (the registry
/// deregistration the composition root injects) records into the same event
/// log, so the purge→deregister ordering is observable. The workspace
/// (`config_path`'s parent) is scaffolded like a real greenfield project so
/// the endpoint's disk cleanup has something to remove.
struct Fixture {
    state: AppState,
    /// The project workspace directory a successful delete must remove.
    workspace: std::path::PathBuf,
    /// Keeps the tempdir alive for the whole test.
    _root: tempfile::TempDir,
}

async fn fixture(events: EventLog, fail_delete: bool) -> Fixture {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = root.path().join(PID);
    std::fs::create_dir_all(workspace.join("state")).expect("scaffold state dir");
    std::fs::write(workspace.join("coxagent.json"), "{}").expect("scaffold config");
    let handle = ProjectHandle {
        id: PID.to_owned(),
        name: "Demo".to_owned(),
        alias: "demo".to_owned(),
        store: RecordingStore::new(Arc::clone(&events), fail_delete),
        runner: Arc::new(RunnerHandle::default()),
        config_path: workspace.join("coxagent.json"),
        engine: Arc::new(store_rpc_test_support::UnusedEngine),
        work_dir: workspace.join("codebase"),
        outbox: None,
        budget: Arc::new(Mutex::new(BudgetCaps::default())),
        context_path: workspace.join("state").join("project_context.md"),
        forge: None,
        deploy: None,
        storage: None,
        files: None,
        deps_discovery: None,
    };
    let remover: ProjectRemover = {
        let events = Arc::clone(&events);
        Arc::new(move |_id| {
            let events = Arc::clone(&events);
            Box::pin(async move {
                events.lock().expect("event log").push("deregister");
                Ok(())
            })
        })
    };
    let state = build_state(
        vec![handle],
        Arc::new(MemoryAuditSink::default()),
        HubExtras {
            hub_dir: Some(root.path().to_path_buf()),
            remover: Some(remover),
            ..Default::default()
        },
    )
    .await;
    Fixture {
        state,
        workspace,
        _root: root,
    }
}

/// The deployed shape: the delete route behind `auth_mw`, exactly as
/// `serve_full` layers it.
fn delete_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/projects/:pid",
            axum::routing::delete(delete_project_ep),
        )
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state)
}

async fn delete_project(router: Router, pid: &str) -> axum::response::Response {
    router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/projects/{pid}"))
                .body(Body::empty())
                .expect("well-formed request"),
        )
        .await
        .expect("in-memory request")
}

#[tokio::test]
async fn ac1_delete_purges_the_stored_state_then_deregisters() {
    let events: EventLog = Arc::new(Mutex::new(Vec::new()));
    let fx = fixture(events.clone(), false).await;
    let router = delete_router(fx.state);

    let resp = delete_project(router, PID).await;
    assert_eq!(resp.status(), StatusCode::OK, "a clean delete must succeed");

    let log = events.lock().expect("event log").clone();
    assert_eq!(
        log.iter().filter(|e| **e == "purge").count(),
        1,
        "the persisted state must be purged exactly once: {log:?}"
    );
    assert_eq!(
        log.first(),
        Some(&"purge"),
        "the purge must run BEFORE the registry deregistration, so a failure \
         aborts while the project is still re-registrable: {log:?}"
    );
    assert!(
        !fx.workspace.exists(),
        "the workspace scaffolding must be gone by the time the 200 is answered"
    );
}

#[tokio::test]
async fn ac1_deleted_project_is_gone_from_the_hub() {
    let events: EventLog = Arc::new(Mutex::new(Vec::new()));
    let fx = fixture(events, false).await;
    let router = delete_router(fx.state);

    let resp = delete_project(router.clone(), PID).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // A second delete of the same id must not find a project anymore.
    let resp = delete_project(router, PID).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "a deleted project must not be deletable twice — it is gone"
    );
}

#[tokio::test]
async fn ac2_failed_purge_aborts_the_whole_delete_flow() {
    let events: EventLog = Arc::new(Mutex::new(Vec::new()));
    let fx = fixture(events.clone(), true).await;
    let router = delete_router(fx.state);

    let resp = delete_project(router, PID).await;
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a 200 while the stored state survives is exactly the resurrection bug — \
         the flow must abort instead"
    );

    let log = events.lock().expect("event log").clone();
    assert_eq!(
        log,
        vec!["purge"],
        "the purge must be attempted, but the flow must stop there — no \
         deregistration and no disk cleanup after a failed purge: {log:?}"
    );
    assert!(
        fx.workspace.exists(),
        "a failed purge must abort before the workspace cleanup too"
    );
}
