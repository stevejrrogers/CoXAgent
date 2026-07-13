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
    pub role: AuthRole,
}

/// Metadata about a minted API token (never the secret itself).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    pub label: String,
    pub role: AuthRole,
    /// RFC3339 creation timestamp.
    pub created: String,
}

/// Authentication boundary: verify credentials, mint/resolve session tokens,
/// and manage long-lived API tokens for service accounts.
#[async_trait]
pub trait AuthPort: Send + Sync {
    /// Verify `username`/`password`; on success return an opaque session token.
    async fn login(&self, username: &str, password: &str) -> Option<String>;

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
}
