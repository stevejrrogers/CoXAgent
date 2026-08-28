// CXA-F029 guard tests — `POST /api/projects/:pid/store`: the STALE-WRITE wire
// contract (AC4).
//
// `op=version` hands the caller the store's current revision; `op=save` may
// carry it back as `revision`. A save against an out-of-date revision answers
// `409 {conflict}` and leaves newer state untouched; retrying against the
// fresh revision commits with `200 {ok:true}`.
//
// The fixture is a pure in-memory store carrying SqlStateStore's revision
// contract (CXA-F003): baseline revision 0, every guarded save bumps, a stale
// `Some(rev)` conflicts. The BACKEND semantics are pinned against real
// Postgres in `crates/infrastructure/tests/sql_store_contract.rs`; this file
// pins the ENDPOINT wire contract — revision passthrough, Conflict→409
// mapping, no clobber, converging retry — which is what the acceptance
// criterion describes. (The hub's default JSON backend is documented
// last-write-wins: `StateStorePort::save_expecting`'s default.)
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]
use super::*;
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use store_rpc_test_support::{app_with, body_text, deployed_router, post_store};

/// In-memory [`StateStorePort`] with SqlStateStore's guarded-write semantics
/// (see `persist_at_revision`): `Some(rev)` is a compare-and-set, `None` falls
/// back to last-write-wins, every successful persist bumps the revision, and
/// an absent row exposes baseline `0`. Only the four methods the wire contract
/// exercises are overridden — the port's defaults cover the rest.
struct VersionedStore {
    state: Mutex<ProjectState>,
    revision: AtomicI64,
}

impl VersionedStore {
    fn new() -> Self {
        Self {
            state: Mutex::new(ProjectState::default()),
            revision: AtomicI64::new(0),
        }
    }

    fn revision(&self) -> i64 {
        self.revision.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl StateStorePort for VersionedStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().expect("lock").clone())
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        self.save_expecting(state, None).await
    }

    async fn save_expecting(
        &self,
        state: &ProjectState,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        let mut guard = self.state.lock().expect("lock");
        if let Some(rev) = expected_revision {
            if self.revision() != rev {
                return Err(PortError::Conflict(
                    "state changed since last read (concurrent writer)".to_owned(),
                ));
            }
        }
        *guard = state.clone();
        self.revision.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn current_version(&self) -> Result<Option<i64>, PortError> {
        Ok(Some(self.revision()))
    }
}

/// POST op=save with a full state snapshot guarded by `revision`.
async fn save_at(router: Router, revision: i64, state: &ProjectState) -> axum::response::Response {
    let data = serde_json::to_string(state).expect("serialize state");
    post_store(
        router,
        "save",
        serde_json::json!({ "data": data, "revision": revision }),
        None,
        None,
    )
    .await
}

/// The revision the runner captures with op=version must be the same token
/// op=save guards with — baseline 0 on an unwritten project, bumped by every
/// guarded save (the CXA-F003 contract, now required over REST).
#[tokio::test]
async fn version_reports_the_revision_the_stale_write_guard_expects() {
    let store = Arc::new(VersionedStore::new());
    let app = app_with(None, store.clone()).await;
    let router = deployed_router(app);

    let resp = post_store(router.clone(), "version", serde_json::json!({}), None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let revision = serde_json::from_str::<serde_json::Value>(&body_text(resp).await)
        .expect("version body")
        .get("revision")
        .and_then(serde_json::Value::as_i64);
    assert_eq!(
        revision,
        Some(0),
        "an unwritten project exposes baseline revision 0 for stale-write protection"
    );

    let state = ProjectState::default();
    let resp = save_at(router.clone(), 0, &state).await;
    let status = resp.status();
    let body = body_text(resp).await;
    assert_eq!(status, StatusCode::OK, "saving against revision 0 — got: {body}");

    let resp = post_store(router, "version", serde_json::json!({}), None, None).await;
    let revision = serde_json::from_str::<serde_json::Value>(&body_text(resp).await)
        .expect("version body")
        .get("revision")
        .and_then(serde_json::Value::as_i64);
    assert_eq!(
        revision,
        Some(1),
        "a guarded save bumps the revision the next writer must capture"
    );
}

/// The full read-modify-write story: a writer holding an out-of-date revision
/// gets 409 {conflict} without overwriting the newer state, and retrying
/// against the fresh revision commits with 200 {ok:true}.
#[tokio::test]
async fn stale_save_conflicts_and_the_newer_state_survives_a_fresh_retry() {
    let store = Arc::new(VersionedStore::new());
    let app = app_with(None, store.clone()).await;
    let router = deployed_router(app);

    // Writer A captured rev 0 (baseline) and commits -> revision 1.
    let first = ProjectState {
        current_version: coxagent_domain::SemVer::new(2, 0, 0),
        ..ProjectState::default()
    };
    let resp = save_at(router.clone(), 0, &first).await;
    let status = resp.status();
    let body = body_text(resp).await;
    assert_eq!(status, StatusCode::OK, "first writer @rev 0 — got: {body}");

    // Writer B captured rev 1 and races ahead -> revision 2.
    let theirs = ProjectState {
        current_version: coxagent_domain::SemVer::new(2, 1, 0),
        ..ProjectState::default()
    };
    let resp = save_at(router.clone(), 1, &theirs).await;
    let status = resp.status();
    let body = body_text(resp).await;
    assert_eq!(status, StatusCode::OK, "concurrent writer @rev 1 — got: {body}");

    // Writer A still holds rev 1, but the store is at 2: a stale write must
    // conflict instead of silently clobbering B's update.
    let ours_stale = ProjectState {
        current_version: coxagent_domain::SemVer::new(2, 2, 0),
        ..ProjectState::default()
    };
    let resp = save_at(router.clone(), 1, &ours_stale).await;
    let status = resp.status();
    let body = body_text(resp).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a stale revision must answer 409 {{conflict}} — got: {body}"
    );
    assert!(
        body.contains("conflict"),
        "the 409 body must carry the conflict — got: {body}"
    );
    assert_eq!(
        store
            .load()
            .await
            .expect("reload after rejected write")
            .current_version,
        coxagent_domain::SemVer::new(2, 1, 0),
        "the rejected write must not overwrite the newer state"
    );

    // Retrying the read-modify-write against the fresh revision converges.
    let resp = post_store(router.clone(), "version", serde_json::json!({}), None, None).await;
    let fresh = serde_json::from_str::<serde_json::Value>(&body_text(resp).await)
        .expect("version body")
        .get("revision")
        .and_then(serde_json::Value::as_i64);
    assert_eq!(fresh, Some(2), "the retry needs the fresh revision");
    let resp = save_at(router, fresh.expect("fresh revision"), &ours_stale).await;
    let status = resp.status();
    let body = body_text(resp).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "retry against the fresh revision must commit — got: {body}"
    );
    assert_eq!(
        store.load().await.expect("reload after retry").current_version,
        coxagent_domain::SemVer::new(2, 2, 0),
        "the retried write must land"
    );
}
