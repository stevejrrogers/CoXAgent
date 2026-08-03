//! Authentication & authorization port. The concrete store (password hashing,
//! session tokens) lives in infrastructure; use cases and the server depend
//! only on this boundary. When no `AuthPort` is wired the app runs open
//! (single-user local mode); the hub wires one to enforce RBAC.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A user's organisational role, which also gates access.
///
/// Tiers, from most to least privileged:
/// - `Admin` — full control (runner, onboarding, config, users, everything).
/// - **Lead tier** (`Director`, `Manager`, `TechLead`, `DsLead`, `DaLead`) —
///   may write (create tickets, run agents, git actions), review code, and
///   create chat channels.
/// - **Member tier** (`Ba`, `Fe`, `Be`, `Aie`, `Ds`, `Da`, `De`) — may chat and
///   view, but not mutate project data or create channels.
/// - `Reviewer` / `Viewer` — legacy roles kept for existing accounts; Reviewer
///   may review code, Viewer is read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthRole {
    /// Hub owner: full power across every space — creates/edits spaces,
    /// promotes admins, sees everything.
    Super,
    Admin,
    // Lead tier
    Director,
    Manager,
    TechLead,
    DsLead,
    DaLead,
    // Member tier (individual contributors)
    Ba,
    Fe,
    Be,
    Aie,
    Ds,
    Da,
    De,
    // Legacy
    Reviewer,
    Viewer,
}

impl AuthRole {
    /// Whether this role is in the lead tier (Director/Manager/*.Lead). Leads may
    /// write, review, and create channels.
    #[must_use]
    pub fn is_lead(self) -> bool {
        matches!(
            self,
            Self::Director | Self::Manager | Self::TechLead | Self::DsLead | Self::DaLead
        )
    }

    /// Whether this role may perform mutating (write) actions on project data
    /// and settings. Every real role can write; only the legacy read-only
    /// `Viewer` cannot.
    #[must_use]
    pub fn can_write(self) -> bool {
        !matches!(self, Self::Viewer)
    }

    /// Whether this role may act on code review (approve / merge / close /
    /// preview pull requests): Super, Admin, the lead tier, and the legacy
    /// Reviewer. Deliberately NARROWER than [`Self::can_write`] — a member-tier
    /// contributor may write project data but must not sign off on, merge, or
    /// force-merge a pull request (COX-B038).
    #[must_use]
    pub fn can_review(self) -> bool {
        matches!(self, Self::Super | Self::Admin | Self::Reviewer) || self.is_lead()
    }

    /// Whether this role may create chat channels: Super, Admin, and the lead tier.
    #[must_use]
    pub fn can_create_channel(self) -> bool {
        matches!(self, Self::Super | Self::Admin) || self.is_lead()
    }

    /// Whether this role may access management surfaces — project Settings and
    /// user administration: Admin and the lead tier (Director/Manager/*.Lead).
    /// Member-tier roles (BA/FE/BE/…) can work and chat but not administer.
    #[must_use]
    pub fn can_manage(self) -> bool {
        matches!(self, Self::Super | Self::Admin) || self.is_lead()
    }

    /// Hub-wide super admin (cross-space power).
    #[must_use]
    pub fn is_super(self) -> bool {
        matches!(self, Self::Super)
    }

    /// Lowercase wire label. Compound roles collapse dots (e.g. `"techlead"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Super => "super",
            Self::Admin => "admin",
            Self::Director => "director",
            Self::Manager => "manager",
            Self::TechLead => "techlead",
            Self::DsLead => "dslead",
            Self::DaLead => "dalead",
            Self::Ba => "ba",
            Self::Fe => "fe",
            Self::Be => "be",
            Self::Aie => "aie",
            Self::Ds => "ds",
            Self::Da => "da",
            Self::De => "de",
            Self::Reviewer => "reviewer",
            Self::Viewer => "viewer",
        }
    }

    /// Human-friendly display label (e.g. `"Tech.Lead"`, `"BA"`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Super => "Super Admin",
            Self::Admin => "Admin",
            Self::Director => "Director",
            Self::Manager => "Manager",
            Self::TechLead => "Tech.Lead",
            Self::DsLead => "DS.Lead",
            Self::DaLead => "DA.Lead",
            Self::Ba => "BA",
            Self::Fe => "FE",
            Self::Be => "BE",
            Self::Aie => "AIE",
            Self::Ds => "DS",
            Self::Da => "DA",
            Self::De => "DE",
            Self::Reviewer => "Reviewer",
            Self::Viewer => "Viewer",
        }
    }

    /// Parse a wire label; unknown values fall back to the least-privileged
    /// [`AuthRole::Viewer`]. Accepts dotted/spaced forms (e.g. `"tech.lead"`).
    #[must_use]
    pub fn from_str_lenient(s: &str) -> Self {
        let norm = s
            .trim()
            .to_ascii_lowercase()
            .replace(['.', ' ', '-', '_'], "");
        match norm.as_str() {
            "super" | "superadmin" => Self::Super,
            "admin" => Self::Admin,
            "director" => Self::Director,
            "manager" => Self::Manager,
            "techlead" => Self::TechLead,
            "dslead" => Self::DsLead,
            "dalead" => Self::DaLead,
            "ba" => Self::Ba,
            "fe" => Self::Fe,
            "be" => Self::Be,
            "aie" => Self::Aie,
            "ds" => Self::Ds,
            "da" => Self::Da,
            "de" => Self::De,
            "reviewer" => Self::Reviewer,
            _ => Self::Viewer,
        }
    }

    /// All roles assignable in the UI, most privileged first.
    #[must_use]
    pub fn all() -> &'static [AuthRole] {
        &[
            Self::Admin,
            Self::Director,
            Self::Manager,
            Self::TechLead,
            Self::DsLead,
            Self::DaLead,
            Self::Ba,
            Self::Fe,
            Self::Be,
            Self::Aie,
            Self::Ds,
            Self::Da,
            Self::De,
        ]
    }
}

#[cfg(test)]
mod role_tests {
    use super::AuthRole;

    #[test]
    fn write_and_channel_capabilities() {
        assert!(AuthRole::Admin.can_write() && AuthRole::Admin.can_create_channel());
        // Every real role can write now.
        for r in AuthRole::all() {
            assert!(r.can_write(), "{} should write", r.as_str());
        }
        // Only Admin + lead tier create channels.
        assert!(AuthRole::TechLead.can_create_channel());
        assert!(AuthRole::Director.can_create_channel());
        assert!(!AuthRole::Ba.can_create_channel());
        assert!(!AuthRole::De.can_create_channel());
        // Legacy Viewer is read-only.
        assert!(!AuthRole::Viewer.can_write());
    }

    /// Regression for COX-B038: `can_review` had been written as
    /// `can_write() || matches!(self, Reviewer)`, which is mathematically
    /// identical to `can_write()` — so the PR-action gate granted nothing
    /// beyond ordinary write access and any non-Viewer could force-merge.
    #[test]
    fn review_is_admin_leads_reviewer_only_not_every_writer() {
        assert!(AuthRole::Super.can_review());
        assert!(AuthRole::Admin.can_review());
        assert!(AuthRole::Reviewer.can_review());
        // The whole lead tier reviews.
        assert!(AuthRole::Director.can_review());
        assert!(AuthRole::Manager.can_review());
        assert!(AuthRole::TechLead.can_review());
        assert!(AuthRole::DsLead.can_review());
        assert!(AuthRole::DaLead.can_review());
        // Member tier can write but must NOT merge/close/preview a PR.
        for r in [
            AuthRole::Ba,
            AuthRole::Fe,
            AuthRole::Be,
            AuthRole::Aie,
            AuthRole::Ds,
            AuthRole::Da,
            AuthRole::De,
        ] {
            assert!(r.can_write(), "{} should still write", r.as_str());
            assert!(!r.can_review(), "{} must not review PRs", r.as_str());
        }
        assert!(!AuthRole::Viewer.can_review());
    }

    /// The guard that makes the bug un-reintroducible: review must stay a
    /// STRICT subset of write. If someone re-widens `can_review` to every
    /// writer, the "strictly narrower" assertion fails.
    #[test]
    fn review_is_a_strict_subset_of_write() {
        let mut narrower_somewhere = false;
        for r in AuthRole::all() {
            if r.can_review() {
                assert!(
                    r.can_write(),
                    "{} reviews but cannot write — review must imply write",
                    r.as_str()
                );
            } else if r.can_write() {
                narrower_somewhere = true;
            }
        }
        assert!(
            narrower_somewhere,
            "can_review() grants nothing beyond can_write() — the PR gate is a no-op"
        );
    }

    #[test]
    fn review_is_admin_leads_reviewer_only_not_every_writer() {
        assert!(AuthRole::Super.can_review());
        assert!(AuthRole::Admin.can_review());
        assert!(AuthRole::TechLead.can_review());
        assert!(AuthRole::Director.can_review());
        assert!(AuthRole::Reviewer.can_review());
        // Member tier can write but must not be able to review/merge/close PRs.
        for r in [
            AuthRole::Ba,
            AuthRole::Fe,
            AuthRole::Be,
            AuthRole::Aie,
            AuthRole::Ds,
            AuthRole::Da,
            AuthRole::De,
        ] {
            assert!(r.can_write(), "{} should still write", r.as_str());
            assert!(!r.can_review(), "{} must not review PRs", r.as_str());
        }
        assert!(!AuthRole::Viewer.can_review());
    }

    #[test]
    fn manage_is_admin_plus_leads_only() {
        assert!(AuthRole::Admin.can_manage());
        assert!(AuthRole::Manager.can_manage());
        assert!(AuthRole::Director.can_manage());
        assert!(AuthRole::TechLead.can_manage());
        assert!(AuthRole::DsLead.can_manage());
        // Member tier cannot access management surfaces.
        assert!(!AuthRole::Ba.can_manage());
        assert!(!AuthRole::Fe.can_manage());
        assert!(!AuthRole::De.can_manage());
    }

    #[test]
    fn super_is_the_only_hub_wide_role() {
        assert!(AuthRole::Super.is_super());
        assert!(AuthRole::Super.can_manage());
        assert!(AuthRole::Super.can_write());
        // No other role — including Admin — is hub-wide super.
        for r in AuthRole::all() {
            if *r != AuthRole::Super {
                assert!(!r.is_super(), "{} must not be super", r.as_str());
            }
        }
        assert_eq!(AuthRole::Super.as_str(), "super");
        assert_eq!(AuthRole::Super.label(), "Super Admin");
        // Both accepted spellings parse to Super; serde wire form round-trips.
        assert_eq!(AuthRole::from_str_lenient("super"), AuthRole::Super);
        assert_eq!(AuthRole::from_str_lenient("SuperAdmin"), AuthRole::Super);
        assert_eq!(AuthRole::from_str_lenient("super_admin"), AuthRole::Super);
        let json = serde_json::to_string(&AuthRole::Super).unwrap();
        assert_eq!(json, "\"super\"");
        assert_eq!(
            serde_json::from_str::<AuthRole>("\"super\"").unwrap(),
            AuthRole::Super
        );
    }

    #[test]
    fn round_trips_wire_labels() {
        for r in AuthRole::all() {
            assert_eq!(AuthRole::from_str_lenient(r.as_str()), *r);
        }
        // Dotted/spaced human forms parse too.
        assert_eq!(AuthRole::from_str_lenient("Tech.Lead"), AuthRole::TechLead);
        assert_eq!(AuthRole::from_str_lenient("DA.Lead"), AuthRole::DaLead);
        assert_eq!(AuthRole::from_str_lenient("nonsense"), AuthRole::Viewer);
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
