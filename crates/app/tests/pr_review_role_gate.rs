//! COX-B038 regression guard: the PR review-action gate in `auth_mw` must
//! actually narrow access.
//!
//! The gate applies `AuthRole::can_review()` to every action under
//! `/api/projects/:pid/prs/:num/:action` (merge, request-changes, close,
//! preview, preview-stop, force-merge). `can_review()` used to be
//! `can_write() || Reviewer`, i.e. "any role except Viewer" — mathematically
//! identical to the ordinary write gate, so a member-tier user (BA/PO/QA/SM)
//! could force-merge a PR (bypassing CI per the `require_ci` feature) or spin
//! up a preview deploy.
//!
//! The unit test in `coxagent_application::auth` pins the predicate; this one
//! pins the HTTP behaviour end to end, because the predicate is only half of
//! it — the route also has to reach the review branch of the gate at all.
//!
//! The principal comes from a stub [`AuthPort`] rather than a real store so
//! that the assertions are about the gate and nothing else: the session user's
//! role and project membership are set directly, with no login/2FA/persistence
//! in the way.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::auth::{
    AuthPort, AuthRole, AuthUser, LoginResult, SessionInfo, TokenInfo,
};

/// Fixed, high, and unique to this test file so it does not race the other
/// hub-booting tests in this crate for a port.
const PORT: u16 = 47_711;

/// The project the stub users are members of. Deliberately NOT registered with
/// the hub: a request that clears the role gate falls through to
/// `pr_action_ep`'s project lookup and answers 404, which is exactly the
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

/// POST a PR action as `role`, returning `(status, body)`.
async fn pr_action(role: AuthRole, action: &str) -> (u16, String) {
    let url = format!("http://127.0.0.1:{PORT}/api/projects/{PID}/prs/7/{action}");
    let resp = reqwest::Client::new()
        .post(&url)
        .header(
            reqwest::header::COOKIE,
            format!("cox_session={}", StubAuth::token(role)),
        )
        .json(&serde_json::json!({ "comment": "" }))
        .send()
        .await
        .expect("request");
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}

/// Every action the `:action` route dispatches to. Force-merge is the sharpest
/// — it bypasses CI and merges without review sign-off — but a preview action
/// deploys the PR branch, so none of these are ordinary writes.
const ACTIONS: [&str; 6] = [
    "merge",
    "request-changes",
    "close",
    "preview",
    "preview-stop",
    "force-merge",
];

/// One hub, one port: these assertions share a process-wide TCP bind, so they
/// live in a single `#[tokio::test]` rather than racing each other.
#[tokio::test]
async fn pr_actions_are_reviewer_only_not_open_to_every_writer() {
    let _dir = boot().await;

    // The non-dev member tier can write but must not sign off a PR: BA, PO, QA
    // and SM review nothing (per the gate map "dev và SA duyệt"), and Viewer is
    // read-only entirely. `can_write()` was once true for all of them except
    // Viewer — which is exactly why every one of these used to get through.
    for role in [
        AuthRole::Ba,
        AuthRole::Po,
        AuthRole::Qa,
        AuthRole::Sm,
        AuthRole::Viewer,
    ] {
        for action in ACTIONS {
            let (status, body) = pr_action(role, action).await;
            assert_eq!(
                status,
                403,
                "{} must not reach /prs/{action}",
                role.as_str()
            );
            assert!(
                body.contains("insufficient role"),
                "/prs/{action} must be refused by the role gate, not by \
                 project membership — got: {body}"
            );
        }
    }

    // Admin, the lead tier, the legacy Reviewer, the SA, and every developer
    // keep the access the gate documents: they clear RBAC and fall through to
    // the handler, which 404s on the unregistered project.
    for role in [
        AuthRole::Super,
        AuthRole::Admin,
        AuthRole::Reviewer,
        AuthRole::Sa,
        AuthRole::Director,
        AuthRole::Manager,
        AuthRole::TechLead,
        AuthRole::DsLead,
        AuthRole::DaLead,
        AuthRole::Fe,
        AuthRole::Fe,
        AuthRole::Be,
        AuthRole::Aie,
        AuthRole::Ds,
        AuthRole::Da,
        AuthRole::De,
    ] {
        for action in ACTIONS {
            let (status, body) = pr_action(role, action).await;
            assert_ne!(
                status,
                403,
                "{} must clear the gate for /prs/{action} — got: {body}",
                role.as_str()
            );
        }
    }
}

