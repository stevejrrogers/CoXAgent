//! P5a guard: `POST /api/projects/:pid/store` — the REST state-store endpoint
//! a fronted runner uses instead of direct Postgres/Redis — must require a
//! valid principal whenever auth is configured, and stay open when it is not.
//!
//! The traffic is behind `auth_mw` like every other API route; these tests pin
//! that wiring end to end so a future move of `/store` outside the middleware
//! cannot silently reopen state mutation without auth.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::auth::{
    AuthPort, AuthRole, AuthUser, LoginResult, SessionInfo, TokenInfo,
};

/// Fixed, high, and unique to this test file so it does not race the other
/// hub-booting tests in this crate for a port.
const PORT: u16 = 47_712;

/// The project the stub users are members of. Deliberately NOT registered with
/// the hub: a request that clears the role gate falls through to
/// `store_rpc_ep`'s project lookup and answers 404, which is exactly the
/// signal this test needs — "RBAC did not stop it".
const PID: &str = "demo";

/// Session store with no I/O: a fixed token per role, each user a member of
/// [`PID`]. Everything else is unreachable in this test and answers "no".
struct StubAuth;

impl StubAuth {
    /// The session token handed to a user of `role` — the role's wire label,
    /// so a failing assertion names the role that got through.
    fn token(role: AuthRole) -> String {
        format!("session-{}", role.as_str())
    }
}

#[async_trait]
impl AuthPort for StubAuth {
    async fn login(&self, _u: &str, _p: &str, _t: Option<&str>) -> LoginResult {
        LoginResult::Denied
    }

    async fn user_for(&self, token: &str) -> Option<AuthUser> {
        let role = token
            .strip_prefix("session-")
            .map(AuthRole::from_str_lenient)?;
        Some(AuthUser {
            username: format!("user-{}", role.as_str()),
            name: String::new(),
            email: String::new(),
            role,
            projects: vec![PID.to_owned()],
        })
    }

    async fn logout(&self, _token: &str) {}

    async fn principal_for_bearer(&self, _token: &str) -> Option<AuthUser> {
        None
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

/// Boot a hub whose only auth store is [`StubAuth`], and wait for it to answer.
async fn boot() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let auth: Arc<dyn AuthPort> = Arc::new(StubAuth);
    let extras = coxagent_presentation::HubExtras {
        auth: Some(auth),
        hub_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
        Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
    tokio::spawn(coxagent_presentation::serve_full(
        vec![],
        PORT,
        audit,
        extras,
    ));

    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{PORT}/api/health");
    for _ in 0..50 {
        if client.get(&health).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    dir
}

/// POST a store op as `role`, returning `(status, body)`. `None` sends no
/// session cookie at all — the unauthenticated case.
async fn store_op(role: Option<AuthRole>, op: &str) -> (u16, String) {
    let url = format!("http://127.0.0.1:{PORT}/api/projects/{PID}/store?op={op}");
    let mut builder = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({}));
    if let Some(role) = role {
        builder = builder.header(
            reqwest::header::COOKIE,
            format!("cox_session={}", StubAuth::token(role)),
        );
    }
    let resp = builder.send().await.expect("request");
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}

/// Every operation this single endpoint can dispatch onto the state store.
const OPS: [&str; 4] = ["load", "save", "claim_ticket", "acquire_leader"];

/// One hub, one port: these assertions share a process-wide TCP bind, so they
/// live in a single `#[tokio::test]` rather than racing each other.
#[tokio::test]
async fn store_requires_a_principal_and_member_writes_clear_it() {
    let _dir = boot().await;

    for op in OPS {
        let (status, body) = store_op(None, op).await;
        assert_eq!(status, 401, "unauthenticated /store?op={op} — got: {body}");
    }

    for role in [
        AuthRole::Ba,
        AuthRole::Fe,
        AuthRole::Be,
        AuthRole::Aie,
        AuthRole::Ds,
        AuthRole::Da,
        AuthRole::De,
    ] {
        let (status, body) = store_op(Some(role), "load").await;
        assert_ne!(
            status,
            401,
            "{} must clear /store auth — got: {body}",
            role.as_str()
        );
    }
}
