// Shared in-process harness for the `/store` guard-test files: a stub auth
// store, a call-counting state store, and the deployed / bare-handler router
// shapes. Pure — no hub process, no host harness, no TCP port; requests are
// answered via `tower::ServiceExt::oneshot`, the same style as
// `cors_rate_limit_tests`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]

use super::*;
use axum::body::Body;
use coxagent_application::auth::{AuthRole, AuthUser, LoginResult, SessionInfo, TokenInfo};
use coxagent_application::config::BudgetCaps;
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;
use coxagent_infrastructure::MemoryAuditSink;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tower::ServiceExt;

/// The project every request in these files addresses.
pub(super) const PID: &str = "demo";
/// A second project: the outside session's ONLY membership, so any access to
/// [`PID`] is cross-project by construction.
pub(super) const OTHER_PID: &str = "elsewhere";
/// Alice's session — member of [`PID`].
pub(super) const INSIDE_SESSION: &str = "session-inside";
/// Carol's session — member-tier (BE) member of [`PID`]: can work the
/// project but must never administer its share links (CXA-F069).
pub(super) const MEMBER_SESSION: &str = "session-member";
/// Mallory's session — member of [`OTHER_PID`] only, authenticated globally.
pub(super) const OUTSIDE_SESSION: &str = "session-outside";
/// A lead-tier session — manage rights hub-wide, but member of [`OTHER_PID`]
/// only, so /store access to [`PID`] trips the membership branch of the
/// handler gate (distinct from the manage-bar branch the member-tier outsider
/// trips).
pub(super) const LEAD_ELSEWHERE_SESSION: &str = "session-lead-elsewhere";
/// Alice's bearer token. Every other bearer is invalid or expired.
pub(super) const INSIDE_BEARER: &str = "bearer-inside";
/// A fixed RFC3339 instant for lease arguments.
pub(super) const NOW: &str = "2026-08-28T00:00:00Z";

/// Mint a stub principal. The three the harness actually uses: Alice (member
/// of [`PID`], Admin — hub-wide by design), Mallory (member of [`OTHER_PID`]
/// only, member tier) and Morgan (member of [`OTHER_PID`] only, manage tier).
/// Mallory and Morgan are authenticated globally yet hold no membership in
/// [`PID`], the exact cross-project case the RBAC gate forbids — they differ
/// in WHICH gate branch refuses them.
pub(super) fn principal(username: &str, role: AuthRole, projects: &[&str]) -> AuthUser {
    AuthUser {
        username: username.to_owned(),
        name: String::new(),
        email: String::new(),
        role,
        projects: projects.iter().map(|p| (*p).to_owned()).collect(),
    }
}

/// Session/bearer store with no IO. Everything it is not explicitly stubbed
/// to allow is unreachable in these tests and answers "no".
pub(super) struct StubAuth;

#[async_trait::async_trait]
impl AuthPort for StubAuth {
    async fn login(&self, _u: &str, _p: &str, _t: Option<&str>) -> LoginResult {
        LoginResult::Denied
    }

    async fn user_for(&self, token: &str) -> Option<AuthUser> {
        match token {
            INSIDE_SESSION => Some(principal("alice", AuthRole::Admin, &[PID])),
            MEMBER_SESSION => Some(principal("carol", AuthRole::Be, &[PID])),
            OUTSIDE_SESSION => Some(principal("mallory", AuthRole::Be, &[OTHER_PID])),
            LEAD_ELSEWHERE_SESSION => Some(principal("morgan", AuthRole::Manager, &[OTHER_PID])),
            _ => None,
        }
    }

    async fn logout(&self, _token: &str) {}

    async fn principal_for_bearer(&self, token: &str) -> Option<AuthUser> {
        if token == INSIDE_BEARER {
            Some(principal("alice", AuthRole::Admin, &[PID]))
        } else {
            None // invalid or expired bearer
        }
    }

    async fn create_token(&self, _label: &str, _role: AuthRole) -> Option<String> {
        None
    }

    async fn list_tokens(&self) -> Vec<TokenInfo> {
        Vec::new()
    }

    async fn revoke_token(&self, _label: &str) -> bool {
        false
    }

    async fn list_users(&self) -> Vec<AuthUser> {
        Vec::new()
    }

    async fn create_user(&self, _u: &str, _p: &str, _r: AuthRole) -> bool {
        false
    }

    async fn delete_user(&self, _username: &str) -> bool {
        false
    }

    async fn assign_project(&self, _username: &str, _pid: &str) -> bool {
        false
    }

    async fn unassign_project(&self, _username: &str, _pid: &str) -> bool {
        false
    }

    async fn enroll_2fa(&self, _username: &str) -> Option<(String, String)> {
        None
    }

    async fn enable_2fa(&self, _username: &str, _code: &str) -> bool {
        false
    }

    async fn disable_2fa(&self, _username: &str) -> bool {
        false
    }

    async fn has_2fa(&self, _username: &str) -> bool {
        false
    }

    async fn sessions_for(&self, _username: &str, _current: &str) -> Vec<SessionInfo> {
        Vec::new()
    }
}

/// A `StateStorePort` that counts every call, so a test can prove the store
/// adapter was never reached — the sharp edge of "instead of forwarding to
/// the store adapter". Semantics otherwise mirror the port's single-runner
/// defaults; behavior is irrelevant here because nothing may call in.
pub(super) struct CountingStore {
    state: Mutex<ProjectState>,
    calls: AtomicUsize,
}

impl CountingStore {
    pub(super) fn seeded(state: ProjectState) -> Self {
        Self {
            state: Mutex::new(state),
            calls: AtomicUsize::new(0),
        }
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl StateStorePort for CountingStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.state.lock().expect("lock").clone())
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.state.lock().expect("lock") = state.clone();
        Ok(())
    }

    async fn save_expecting(
        &self,
        state: &ProjectState,
        _expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        self.save(state).await
    }

    async fn current_version(&self) -> Result<Option<i64>, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }

    async fn claim_ticket(
        &self,
        _id: &coxagent_domain::TicketId,
        _worker: &str,
        _now: &str,
    ) -> Result<bool, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }

    async fn acquire_leader(&self, _worker: &str, _now: &str) -> Result<bool, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }

    async fn claim_stage(
        &self,
        _id: &coxagent_domain::TicketId,
        _stage: &str,
        _worker: &str,
        _now: &str,
    ) -> Result<bool, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }

    async fn release_stage(
        &self,
        _id: &coxagent_domain::TicketId,
        _stage: &str,
        _worker: &str,
    ) -> Result<(), PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn heartbeat_worker(
        &self,
        _worker: &str,
        _role: &str,
        _ticket: &str,
        _caps: &coxagent_application::ports::outbound::WorkerCaps,
        _now: &str,
    ) -> Result<(), PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn workers(
        &self,
    ) -> Result<Vec<coxagent_application::ports::outbound::WorkerEntry>, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    async fn set_desired(&self, _operator: &str, _running: bool) -> Result<(), PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn get_desired(&self, _operator: &str) -> Result<Option<bool>, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }

    async fn acquire_operator(&self, _operator: &str, _instance: &str) -> Result<bool, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

/// Never invoked by the store RPC path under test.
pub(super) struct UnusedEngine;
#[async_trait::async_trait]
impl coxagent_application::ports::outbound::AgentEnginePort for UnusedEngine {
    fn id(&self) -> &'static str {
        "unused"
    }
    async fn run(
        &self,
        _request: coxagent_application::ports::outbound::AgentRequest,
    ) -> Result<coxagent_application::ports::outbound::AgentOutcome, PortError> {
        unreachable!("not called by the store RPC")
    }
}

/// One registered project (id [`PID`]) backed by `store`, in a hub whose auth
/// is `auth`. Lives in a tempdir like every other hub fixture.
pub(super) async fn app_with(
    auth: Option<Arc<dyn AuthPort>>,
    store: Arc<dyn StateStorePort>,
) -> AppState {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = tempfile::tempdir().expect("workdir");
    let handle = ProjectHandle {
        id: PID.to_owned(),
        name: "Demo".to_owned(),
        alias: "demo".to_owned(),
        store,
        runner: Arc::new(RunnerHandle::default()),
        config_path: work.path().join("coxagent.json"),
        engine: Arc::new(UnusedEngine),
        work_dir: work.path().to_path_buf(),
        outbox: None,
        budget: Arc::new(Mutex::new(BudgetCaps::default())),
        context_path: work.path().join("project_context.md"),
        forge: None,
        deploy: None,
        storage: None,
        files: None,
    };
    build_state(
        vec![handle],
        Arc::new(MemoryAuditSink::default()),
        HubExtras {
            auth,
            hub_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
}

/// The deployed shape of the route: the handler behind `auth_mw`, exactly as
/// `serve_full` layers it (route_layer + with_state).
pub(super) fn deployed_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/projects/:pid/store",
            post(store_rpc::store_rpc_ep).get(store_rpc::store_audit_ep),
        )
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state)
}

/// The handler alone — the seam that forwards to the store adapter. The
/// in-handler gate is what keeps /store safe if the route ever moves out from
/// under `auth_mw` (the P5a defense-in-depth promise in store_rpc.rs).
pub(super) fn handler_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/projects/:pid/store",
            post(store_rpc::store_rpc_ep).get(store_rpc::store_audit_ep),
        )
        .with_state(state)
}

/// POST one store op against project `pid` with the given JSON `Args` body
/// and optional `Authorization: <bearer>` / session cookie values.
pub(super) async fn post_store_at(
    router: Router,
    pid: &str,
    op: &str,
    args: serde_json::Value,
    authorization: Option<&str>,
    session: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/projects/{pid}/store?op={op}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(value) = authorization {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    if let Some(token) = session {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={token}"));
    }
    router
        .oneshot(
            builder
                .body(Body::from(args.to_string()))
                .expect("well-formed request"),
        )
        .await
        .expect("in-memory request")
}

/// POST one store op against the shared [`PID`] project — the shape every
/// guard test but the unknown-project criterion needs.
pub(super) async fn post_store(
    router: Router,
    op: &str,
    args: serde_json::Value,
    authorization: Option<&str>,
    session: Option<&str>,
) -> axum::response::Response {
    post_store_at(router, PID, op, args, authorization, session).await
}

/// GET one store op (the read-only audit surface) against project `pid`,
/// with optional `Authorization` / session cookie values.
pub(super) async fn get_store_at(
    router: Router,
    pid: &str,
    op: Option<&str>,
    authorization: Option<&str>,
    session: Option<&str>,
) -> axum::response::Response {
    let query = op.map_or_else(String::new, |op| format!("?op={op}"));
    let mut builder = Request::builder()
        .method("GET")
        .uri(format!("/api/projects/{pid}/store{query}"));
    if let Some(value) = authorization {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    if let Some(token) = session {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={token}"));
    }
    router
        .oneshot(builder.body(Body::empty()).expect("well-formed request"))
        .await
        .expect("in-process request")
}

/// Drain a response body into text for assertions.
pub(super) async fn body_text(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("response body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// One representative `Args` body per operation, in the exact shapes the
/// runner's `RestStateStore` sends (crates/infrastructure/src/state/rest_store.rs).
pub(super) fn all_ops() -> Vec<(&'static str, serde_json::Value)> {
    let beat = serde_json::json!({ "role": "be", "now": NOW }).to_string();
    // A real runner serializes a full ProjectState (RestStateStore::save); a
    // bare "{}" is not one, so the save fixture carries a complete snapshot.
    let full_state = serde_json::to_string(&ProjectState::default()).expect("serialize state");
    vec![
        ("load", serde_json::json!({})),
        ("version", serde_json::json!({})),
        ("save", serde_json::json!({ "data": full_state })),
        (
            "claim_ticket",
            serde_json::json!({ "id": "CXC-F001", "worker": "dev@mac", "now": NOW }),
        ),
        (
            "acquire_leader",
            serde_json::json!({ "worker": "dev@mac", "now": NOW }),
        ),
        (
            "claim_stage",
            serde_json::json!({ "id": "CXC-F001", "stage": "dev", "worker": "dev@mac", "now": NOW }),
        ),
        (
            "release_stage",
            serde_json::json!({ "id": "CXC-F001", "stage": "dev", "worker": "dev@mac" }),
        ),
        (
            "heartbeat",
            serde_json::json!({ "worker": "dev@mac", "data": beat }),
        ),
        ("workers", serde_json::json!({})),
        (
            "set_desired",
            serde_json::json!({ "worker": "operator", "data": "true" }),
        ),
        ("get_desired", serde_json::json!({ "worker": "operator" })),
        (
            "acquire_operator",
            serde_json::json!({ "worker": "operator", "now": "instance-1" }),
        ),
    ]
}
