//! Authentication & authorization port. The concrete store (password hashing,
//! session tokens) lives in infrastructure; use cases and the server depend
//! only on this boundary. When no `AuthPort` is wired the app runs open
//! (single-user local mode); the hub wires one to enforce RBAC.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A coarse access role. `Admin` may mutate (control the runner, onboard, set
/// priorities, reject, comment); `Viewer` is read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthRole {
    Admin,
    Viewer,
}

impl AuthRole {
    /// Whether this role may perform mutating (write) actions.
    #[must_use]
    pub fn can_write(self) -> bool {
        matches!(self, Self::Admin)
    }
}

/// An authenticated principal (never carries the password or its hash).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthUser {
    pub username: String,
    /// Human display name (optional; empty when unset).
    #[serde(default)]
    pub name: String,
    /// Contact email (optional; empty when unset).
    #[serde(default)]
    pub email: String,
    pub role: AuthRole,
    /// Project ids this user is a member of (empty for service accounts and
    /// bare session principals). Populated by [`AuthPort::list_users`].
    #[serde(default)]
    pub projects: Vec<String>,
}

/// One active browser/device session for a signed-in user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Friendly device label, e.g. "Chrome on macOS".
    pub label: String,
    /// RFC3339 time the session was created.
    pub at: String,
    /// Whether this is the caller's own session.
    pub current: bool,
}

/// Metadata about a minted API token (never the secret itself).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    pub label: String,
    pub role: AuthRole,
    /// RFC3339 creation timestamp.
    pub created: String,
}

/// Outcome of a login attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginResult {
    /// Credentials (and 2FA, if enabled) verified — the session token.
    Ok(String),
    /// Password is correct but a valid TOTP code is required to proceed.
    TotpRequired,
    /// Credentials invalid, account locked, or the TOTP code was wrong.
    Denied,
}

/// Authentication boundary: verify credentials, mint/resolve session tokens,
/// manage API tokens, users, and TOTP two-factor enrollment.
#[async_trait]
pub trait AuthPort: Send + Sync {
    /// Verify `username`/`password` (and `totp` when 2FA is enabled for the
    /// account). Returns a [`LoginResult`].
    async fn login(&self, username: &str, password: &str, totp: Option<&str>) -> LoginResult;

    /// Attach a human device label (e.g. "Chrome on macOS") to a session token,
    /// so a user can see where they're signed in. No-op if the token is unknown.
    async fn attach_device(&self, _token: &str, _device: &str) {}

    /// The active sessions for `username`, newest first — for the "your devices"
    /// view. `current_token` is flagged as the caller's own session.
    async fn sessions_for(&self, _username: &str, _current_token: &str) -> Vec<SessionInfo> {
        Vec::new()
    }

    /// Resolve a session token to its principal, or `None` if invalid/expired.
    async fn user_for(&self, token: &str) -> Option<AuthUser>;

    /// Invalidate a session token (logout). No-op if unknown.
    async fn logout(&self, token: &str);

    /// Resolve a bearer API token to its service-account principal.
    async fn principal_for_bearer(&self, token: &str) -> Option<AuthUser>;

    /// Mint an API token (`label`, `role`) and return the secret **once**.
    /// Returns `None` if the label is taken or persistence fails.
    async fn create_token(&self, label: &str, role: AuthRole) -> Option<String>;

    /// List minted tokens (metadata only, never the secret).
    async fn list_tokens(&self) -> Vec<TokenInfo>;

    /// Revoke the token with `label`. Returns whether one was removed.
    async fn revoke_token(&self, label: &str) -> bool;

    /// List user accounts (usernames + roles, never hashes).
    async fn list_users(&self) -> Vec<AuthUser>;

    /// Create (or update the password/role of) a user account. Returns `false`
    /// if the input is invalid or persistence fails.
    async fn create_user(&self, username: &str, password: &str, role: AuthRole) -> bool;

    /// Update a user's profile (display `name`, `email`) and, when `role` is
    /// `Some`, their role. Empty strings clear the corresponding field. Returns
    /// `false` if the user does not exist or persistence fails.
    async fn update_user(
        &self,
        _username: &str,
        _name: &str,
        _email: &str,
        _role: Option<AuthRole>,
    ) -> bool {
        false
    }

    /// Reset a user's password to `password`. Returns `false` if the user does
    /// not exist, the password is empty, or persistence fails.
    async fn set_password(&self, _username: &str, _password: &str) -> bool {
        false
    }

    /// Remove a user account. Returns `false` if it does not exist or removing
    /// it would leave no admin (the last admin cannot be deleted).
    async fn delete_user(&self, username: &str) -> bool;

    /// Add `pid` to `username`'s project memberships (idempotent). Returns
    /// `false` if the user does not exist or persistence fails.
    async fn assign_project(&self, username: &str, pid: &str) -> bool;

    /// Remove `pid` from `username`'s project memberships. Returns whether the
    /// membership existed and was removed.
    async fn unassign_project(&self, username: &str, pid: &str) -> bool;

    /// Begin TOTP enrollment for `username`: generate a pending secret and return
    /// `(secret, otpauth_uri)`. Not active until [`enable_2fa`](Self::enable_2fa).
    async fn enroll_2fa(&self, username: &str) -> Option<(String, String)>;

    /// Activate 2FA for `username` if `code` matches the pending secret.
    async fn enable_2fa(&self, username: &str, code: &str) -> bool;

    /// Turn off 2FA for `username`. Returns whether it was enabled.
    async fn disable_2fa(&self, username: &str) -> bool;

    /// Whether `username` currently has 2FA enabled.
    async fn has_2fa(&self, username: &str) -> bool;
}
