//! CXA-F002 guard: successful sign-in populates the remote-state bearer token
//! environment variable while rejections leave any preexisting value untouched.
//!
//! On an auth-enabled hub, `POST /api/auth/login` calls
//! [`AuthPort::auto_issue_personal_token`] when the password verifies
//! (`LoginResult::Ok`) and feeds the returned secret into
//! `COXAGENT_REMOTE_TOKEN`. On TOTP/password rejection it returns early without
//! ever touching that variable; on hubs with no auth service configured it does
//! nothing at all; and repeat logins must stay idempotent (mint once per user,
//! reuse thereafter without re-issuing).
//!
//! These pins drive that wiring end-to-end against real HTTP routes backed by a
//! scriptable [`HarvestAuth`] stub whose result/harvest behaviour varies per
//! instance.
//!
//! NOTE ON THE ENVIRONMENT-VARIABLE RACE: every assertion here reads or writes
//! process-global `COXAGENT_REMOTE_TOKEN`, yet separate `#[tokio::test]`
//! functions run concurrently inside one process sharing that global state.
//! Everything therefore lives in a SINGLE sequential test function which saves
//! any prior value up front and restores it afterwards — never two concurrent
//! writers anywhere near these assertions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    // This harness deliberately reads/writes process-global env state, but only from one sequential
    // #[tokio::test] body (see module docs), so the usual "affects other threads" lint does not apply.
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::auth::{
    AuthPort, AuthRole, AuthUser, LoginResult, SessionInfo, TokenInfo,
};

/// Ports this file boots hubs on. All fixed, high, and unique to this file so they cannot collide
/// with `store_rpc_auth_gate.rs` (47_712), `pr_review_role_gate.rs` (47_711) or any other test.
const P_HARVEST: u16 = 47_720;
const P_TOTP: u16 = 47_721;
const P_DENIED: u16 = 47_722;
const P_NOAUTH: u16 = 47_723;

/// The process-wide env key whose harvesting is under test.
const TOKEN_KEY: &str = "COXAGENT_REMOTE_TOKEN";

/// A scriptable auth store for login-harvest scenarios. Behaviour is chosen per instance via its fields:
/// - [`result`](Self::result): what a login attempt answers — Ok/TotpRequired/Denied.
/// - [`secret_to_mint`](Self::secret_to_mint): the personal-token secret handed back on first mint;
///   once a username has been harvested it is consumed so repeat logins observe idempotency.
struct HarvestAuth {
    result: LoginResult,
    secret_to_mint: Option<String>,
    issued: Arc<Mutex<Vec<String>>>,
}

impl HarvestAuth {
    fn new(result: LoginResult, secret_to_mint: Option<String>) -> Self {
        Self {
            result,
            secret_to_mint,
            issued: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl AuthPort for HarvestAuth {
    async fn login(&self, _u: &str, _p: &str, _t: Option<&str>) -> LoginResult {
        self.result.clone()
    }

    async fn user_for(&self, _token: &str) -> Option<AuthUser> {
        None
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

    async fn auto_issue_personal_token(&self, username: &str) -> Option<String> {
        let mut issued = self.issued.lock().unwrap();
        if issued.iter().any(|u| u == username) {
            return None;
        }
        let secret = self.secret_to_mint.clone()?;
        issued.push(username.to_owned());
        Some(secret)
    }
}

/// Boot a hub on `port` with an optional auth store, waiting for it to answer.
/// `Some(stub)` enables auth; `None` yields an auth-less hub (AC3).
async fn boot(port: u16, auth: Option<Arc<dyn AuthPort>>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let extras = coxagent_presentation::HubExtras {
        auth,
        hub_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
        Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
    tokio::spawn(coxagent_presentation::serve_full(
        vec![],
        port,
        audit,
        extras,
    ));

    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{port}/api/health");
    for _ in 0..50 {
        if client.get(&health).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    dir
}

/// POST `/api/auth/login` with JSON creds and return (status, body text).
async fn login_req(port: u16, username: &str, password: &str) -> (u16, String) {
    let url = format!("http://127.0.0.1:{port}/api/auth/login");
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({ "username": username, "password": password }))
        .send()
        .await
        .expect("login request");
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}

/// Save + restore guard for the process-global env key so tests never leak a
/// harvested value into sibling processes or re-run state.
struct EnvGuard(Option<String>);

impl EnvGuard {
    fn save() -> Self {
        EnvGuard(std::env::var(TOKEN_KEY).ok())
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(v) => std::env::set_var(TOKEN_KEY, v),
            None => std::env::remove_var(TOKEN_KEY),
        }
    }
}

/// All acceptance criteria in ONE sequential test body: every assertion touches
/// process-global env state, so they must not run concurrently with each other.
#[tokio::test]
async fn login_harvest_populates_and_rejections_leave_preexisting_value_untouched() {
    let _guard = EnvGuard::save();

    // --- AC1 + AC4: successful sign-in populates; repeat sign-in stays idempotent.
    let stub = Arc::new(HarvestAuth::new(
        LoginResult::Ok("session-alice".to_string()),
        Some("secret-abc".to_string()),
    ));
    let harvest_dir = boot(P_HARVEST, Some(stub.clone())).await;

    std::env::remove_var(TOKEN_KEY);
    let (s1, b1) = login_req(P_HARVEST, "alice", "pw").await;
    assert_eq!(s1, 200, "AC1 login ok - got {b1}");
    assert_eq!(
        std::env::var(TOKEN_KEY).ok(),
        Some("secret-abc".to_string()),
        "AC1: successful sign-in populates COXAGENT_REMOTE_TOKEN"
    );

    // Second sign-in: harvest now returns None (already issued) -> no rotation.
    let (s2, b2) = login_req(P_HARVEST, "alice", "pw").await;
    assert_eq!(s2, 200, "repeat login ok - got {b2}");
    assert_eq!(
        std::env::var(TOKEN_KEY).ok(),
        Some("secret-abc".to_string()),
        "AC4: repeat sign-in does not rotate / re-issue the secret"
    );
    drop(harvest_dir);

    // --- AC2a: totp_required leaves preexisting untouched.
    let stub_totp = Arc::new(HarvestAuth::new(LoginResult::TotpRequired, None));
    let _dir2 = boot(P_TOTP, Some(stub_totp)).await;
    std::env::set_var(TOKEN_KEY, "keep-me");
    let (stotp, _btotp) = login_req(P_TOTP, "carol", "pw").await;
    assert_eq!(stotp, 401);
    assert_eq!(
        std::env::var(TOKEN_KEY).unwrap_or_default(),
        "keep-me",
        "AC2a: totp_required must NOT rotate a preexisting value"
    );
    // --- AC2b: invalid credentials leave preexisting untouched.
    let stub_denied = Arc::new(HarvestAuth::new(LoginResult::Denied, None));
    let denied_dir = boot(P_DENIED, Some(stub_denied)).await;
    std::env::set_var(TOKEN_KEY, "keep-me");
    let (sdeny, _bdeny) = login_req(P_DENIED, "eve", "wrong-password").await;
    assert_eq!(sdeny, 401);
    assert_eq!(
        std::env::var(TOKEN_KEY).unwrap_or_default(),
        "keep-me",
        "AC2b: invalid credentials must NOT rotate a preexisting value"
    );
    drop(denied_dir);

    // --- AC3: a hub with NO auth service configured harvests nothing cleanly —
    // it answers open (`auth:false`) and never touches the env var.
    let _dir4 = boot(P_NOAUTH, None).await;
    std::env::set_var(TOKEN_KEY, "keep-me");
    let (snoauth, bnoauth) = login_req(P_NOAUTH, "oscar", "pw").await;
    assert_eq!(snoauth, 200);
    assert!(
        bnoauth.contains("\"auth\":false"),
        "AC3 open hub answers auth:false — got {bnoauth}"
    );
    assert_eq!(
        std::env::var(TOKEN_KEY).unwrap_or_default(),
        "keep-me",
        "AC3: an unauthenticated hub must not touch COXAGENT_REMOTE_TOKEN"
    );
}
