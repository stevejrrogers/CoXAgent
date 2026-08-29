//! Shared harness for the REST state-store contract tests
//! (`rest_state_store_contract.rs`): fixtures the gateway cannot distinguish
//! from the real thing, plus the boot helpers every test uses.
//!
//! Mirrors the in-crate support pattern of presentation's
//! `store_rpc_test_support`: pure doubles over real port types — no fabricated
//! data, no second implementation of anything under test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::auth::{
    AuthPort, AuthRole, AuthUser, LoginResult, SessionInfo, TokenInfo,
};
use coxagent_application::config::BudgetCaps;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::{PortError, ProjectState};
use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};
use coxagent_infrastructure::{RestConfig, RestStateStore};
use coxagent_presentation::{HubExtras, ProjectHandle};

/// The project every hub registers.
pub const PID: &str = "demo";
/// Ports unique across this crate's hub-booting tests (see
/// `store_rpc_auth_gate.rs` 47_712/47_713, `auth_login_harvest.rs` 47_720+).
pub const JSON_PORT: u16 = 47_735;
pub const COORD_PORT: u16 = 47_738;
pub const OCC_PORT: u16 = 47_736;
pub const AUTH_PORT: u16 = 47_737;
/// Fixed RFC3339 instant for lease arguments (leases compare `now` to `now`).
pub const NOW: &str = "2026-08-28T00:00:00Z";
/// The bearer [`StubAuth`] accepts as an Admin member of [`PID`].
pub const ADMIN_BEARER: &str = "bearer-admin";
/// Member-tier bearer (member of [`PID`], but /store writes demand manage tier).
pub const MEMBER_BEARER: &str = "bearer-member";

pub fn sample_ticket(id: &str) -> Ticket {
    Ticket::new(
        TicketId::new(id).expect("valid id"),
        TicketType::Feature,
        "A feature",
        "desc",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket")
}

/// A Bug starts `Open`, the status from which `System` may claim
/// (`Open -> InProgress` is a legal edge; a fresh Feature sits in `Pending`
/// and is NOT claimable yet). This is what makes the claim assertions in the
/// contract test exercise the real transition table, not a failure path.
pub fn sample_bug(id: &str) -> Ticket {
    Ticket::new(
        TicketId::new(id).expect("valid id"),
        TicketType::Bug,
        "A bug",
        "desc",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("valid ticket")
}

struct StubEngine;
#[async_trait]
impl AgentEnginePort for StubEngine {
    fn id(&self) -> &'static str {
        "stub"
    }
    async fn run(&self, _rq: AgentRequest) -> Result<AgentOutcome, PortError> {
        unreachable!("the store gateway never runs an engine")
    }
}

/// In-memory store with the revision tracking [`JsonStateStore`] deliberately
/// leaves at the port defaults — the semantics `SqlStateStore` implements
/// against Postgres (baseline revision 0, stale write -> `Conflict`). Pure,
/// deterministic, no database; the gateway under test cannot tell the
/// difference, which is exactly what the port abstraction promises.
pub struct VersionedStore {
    state: Mutex<ProjectState>,
    revision: Mutex<i64>,
    /// The last `expected_revision` a `save_expecting` received — proves the
    /// caller's revision actually crossed the wire instead of being dropped.
    last_expected: Mutex<Option<i64>>,
}

impl VersionedStore {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ProjectState::default()),
            revision: Mutex::new(0),
            last_expected: Mutex::new(None),
        }
    }

    pub fn last_expected(&self) -> Option<i64> {
        *self.last_expected.lock().unwrap()
    }
}

#[async_trait]
impl StateStorePort for VersionedStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().unwrap().clone())
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        *self.state.lock().unwrap() = state.clone();
        *self.revision.lock().unwrap() += 1;
        Ok(())
    }

    async fn save_expecting(
        &self,
        state: &ProjectState,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        *self.last_expected.lock().unwrap() = expected_revision;
        let head = *self.revision.lock().unwrap();
        if let Some(expected) = expected_revision {
            if expected != head {
                return Err(PortError::Conflict(format!(
                    "revision {expected} is stale, head is {head}"
                )));
            }
        }
        self.save(state).await
    }

    async fn current_version(&self) -> Result<Option<i64>, PortError> {
        Ok(Some(*self.revision.lock().unwrap()))
    }
}

/// The auth hub's session store: one Admin bearer and one member-tier bearer,
/// both members of [`PID`]. Everything else answers "no" — an invalid bearer
/// must be refused exactly like a missing one.
pub struct StubAuth;

fn principal(role: AuthRole) -> AuthUser {
    AuthUser {
        username: format!("user-{}", role.as_str()),
        name: String::new(),
        email: String::new(),
        role,
        projects: vec![PID.to_owned()],
    }
}

#[async_trait]
impl AuthPort for StubAuth {
    async fn login(&self, _u: &str, _p: &str, _t: Option<&str>) -> LoginResult {
        LoginResult::Denied
    }

    async fn user_for(&self, _token: &str) -> Option<AuthUser> {
        None // the REST adapter authenticates with a bearer, never a session
    }

    async fn logout(&self, _token: &str) {}

    async fn principal_for_bearer(&self, token: &str) -> Option<AuthUser> {
        match token {
            ADMIN_BEARER => Some(principal(AuthRole::Admin)),
            MEMBER_BEARER => Some(principal(AuthRole::Be)),
            _ => None,
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

pub fn handle(store: Arc<dyn StateStorePort>) -> ProjectHandle {
    let dir = std::env::temp_dir();
    ProjectHandle {
        id: PID.to_owned(),
        name: "Demo".to_owned(),
        alias: "demo".to_owned(),
        store,
        runner: Arc::new(coxagent_application::use_cases::RunnerHandle::new()),
        config_path: dir.join("coxagent.json"),
        engine: Arc::new(StubEngine),
        work_dir: dir.clone(),
        budget: Arc::new(Mutex::new(BudgetCaps::default())),
        context_path: dir.join("project_context.md"),
        forge: None,
        deploy: None,
        storage: None,
        files: None,
        deps_discovery: None,
    }
}

/// Boot one hub on `port` with the given projects and optional auth; waits
/// until `/api/health` answers, exactly like the crate's other hub fixtures.
pub async fn boot(
    port: u16,
    projects: Vec<ProjectHandle>,
    auth: Option<Arc<dyn AuthPort>>,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let extras = HubExtras {
        auth,
        hub_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
        Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
    tokio::spawn(coxagent_presentation::serve_full(
        projects, port, audit, extras,
    ));

    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{port}/api/health");
    for _ in 0..50 {
        if client.get(&health).send().await.is_ok() {
            return dir;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("hub on {port} never answered /api/health");
}

/// The runner-side adapter pointed at one hub project, as `make_store` builds
/// it from `COXAGENT_REMOTE_STORE_URL` (+ optional `COXAGENT_REMOTE_TOKEN`).
pub fn rest_store(port: u16, token: Option<&str>) -> RestStateStore {
    RestStateStore::new(RestConfig {
        base_url: format!("http://127.0.0.1:{port}"),
        project_id: PID.to_owned(),
        token: token.map(str::to_owned),
        timeout: Duration::from_secs(10),
    })
    .expect("valid config")
}

/// The current UTC instant as RFC3339 — what a live runner stamps its
/// heartbeat with, so the JSON registry's TTL sees a fresh entry.
pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("format now")
}
