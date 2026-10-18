//! `FileAuthService` — the concrete [`AuthPort`]: Argon2 password hashing, a
//! JSON user file, and in-memory session tokens. The user file stores only
//! salted Argon2 hashes, never plaintext; sessions live in memory and expire.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use async_trait::async_trait;
use coxagent_application::auth::{
    AuthPort, AuthRole, AuthUser, LoginResult, SessionInfo, TokenInfo,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Sessions live this long before a token must be re-issued. Long-lived so a
/// hub relaunch (new binary, config change) never signs the operator out — the
/// token is persisted to `sessions.json` and restored on startup.
const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Consecutive failed logins for one account before it is locked.
const MAX_FAILS: u32 = 5;

/// How long an account stays locked after too many failures.
const LOCKOUT: Duration = Duration::from_secs(15 * 60);

/// One stored account: username, Argon2 password hash (PHC string), role, and
/// an optional (base32) TOTP secret — present only when 2FA is enabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredUser {
    username: String,
    hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    email: String,
    role: AuthRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    totp_secret: Option<String>,
    /// Project ids this user is assigned to work on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    projects: Vec<String>,
}

/// One stored API token: label, role, SHA-256 hex of the secret, created-at.
/// The secret itself is never persisted — only its hash. `owner` names the
/// member a PERSONAL token belongs to (CXA-F350): the bearer inherits that
/// account's project memberships, resolved live per request. Empty for
/// service tokens minted by an admin — those stay hub-wide for their role.
/// `serde(default)` keeps pre-existing `auth.json` files loading unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredToken {
    label: String,
    role: AuthRole,
    hash: String,
    created: String,
    #[serde(default)]
    owner: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UserFile {
    users: Vec<StoredUser>,
    #[serde(default)]
    tokens: Vec<StoredToken>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Session {
    user: AuthUser,
    /// Expiry as a Unix timestamp (seconds), so sessions survive a restart.
    expires: u64,
    /// Human device label ("Chrome on macOS"); empty until `attach_device`.
    label: String,
    /// RFC3339 login time.
    at: String,
}

/// Current Unix time in whole seconds.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Failed-login tracking for one account (brute-force throttle).
#[derive(Default)]
struct Attempts {
    fails: u32,
    /// When set, the account is locked until this instant.
    locked_until: Option<Instant>,
}

/// File-backed auth service. Sessions persist to `sessions.json` beside the
/// user file so a restart doesn't sign everyone out.
pub struct FileAuthService {
    path: PathBuf,
    sessions_path: PathBuf,
    users: Mutex<Vec<StoredUser>>,
    tokens: Mutex<Vec<StoredToken>>,
    sessions: Mutex<HashMap<String, Session>>,
    attempts: Mutex<HashMap<String, Attempts>>,
    /// Pending TOTP secrets during enrollment (before the user confirms a code).
    pending_totp: Mutex<HashMap<String, String>>,
}

impl FileAuthService {
    /// Load users and API tokens from `path`. A missing file yields an empty
    /// (locked) store.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be parsed.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file: UserFile = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("bad auth file: {e}"))?,
            Err(_) => UserFile::default(),
        };
        let sessions_path = path.with_file_name("sessions.json");
        // Restore non-expired sessions so a restart doesn't sign users out.
        let sessions: HashMap<String, Session> = std::fs::read_to_string(&sessions_path)
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, Session>>(&t).ok())
            .map(|mut m| {
                let now = now_unix();
                m.retain(|_, s| s.expires > now);
                m
            })
            .unwrap_or_default();
        Ok(Self {
            path: path.to_path_buf(),
            sessions_path,
            users: Mutex::new(file.users),
            tokens: Mutex::new(file.tokens),
            sessions: Mutex::new(sessions),
            attempts: Mutex::new(HashMap::new()),
            pending_totp: Mutex::new(HashMap::new()),
        })
    }

    /// Persist the current users + tokens back to `path`.
    fn persist(&self) -> Result<(), String> {
        let users = self.users.lock().map_err(|e| e.to_string())?.clone();
        let tokens = self.tokens.lock().map_err(|e| e.to_string())?.clone();
        let file = UserFile { users, tokens };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, text).map_err(|e| e.to_string())
    }

    /// Persist active sessions to `sessions.json` (best-effort). Called after
    /// any change to the session map so tokens survive a restart.
    fn persist_sessions(&self) {
        let Ok(sessions) = self.sessions.lock() else {
            return;
        };
        if let Ok(text) = serde_json::to_string(&*sessions) {
            let _ = std::fs::write(&self.sessions_path, text);
        }
    }

    /// Ensure an admin account with `username`/`password` exists in `path`,
    /// creating the file if absent and **updating the hash if the account
    /// already exists**. The environment-provided admin is the source of truth,
    /// so a stale `auth.json` never locks the operator out — re-running with the
    /// intended password always makes it work.
    ///
    /// # Errors
    /// Returns an error if hashing, reading, or writing the file fails.
    pub fn bootstrap_admin(path: &Path, username: &str, password: &str) -> Result<(), String> {
        let hash = hash_password(password)?;
        let mut file: UserFile = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("bad auth file: {e}"))?,
            Err(_) => UserFile::default(),
        };
        match file.users.iter_mut().find(|u| u.username == username) {
            Some(existing) => {
                existing.hash = hash;
                existing.role = AuthRole::Super;
            }
            None => file.users.push(StoredUser {
                username: username.to_owned(),
                hash,
                name: String::new(),
                email: String::new(),
                role: AuthRole::Super,
                totp_secret: None,
                projects: Vec::new(),
            }),
        }
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, text).map_err(|e| e.to_string())
    }

    /// Whether any account is configured. When false the app runs open.
    #[must_use]
    pub fn has_users(&self) -> bool {
        self.users.lock().is_ok_and(|u| !u.is_empty())
    }

    /// Whether `username` is currently locked out (expired locks are cleared).
    fn is_locked(&self, username: &str) -> bool {
        let Ok(mut map) = self.attempts.lock() else {
            return false;
        };
        let Some(a) = map.get_mut(username) else {
            return false;
        };
        match a.locked_until {
            Some(until) if until > Instant::now() => true,
            Some(_) => {
                // Lock expired: reset the counter.
                a.locked_until = None;
                a.fails = 0;
                false
            }
            None => false,
        }
    }

    /// Count a failed attempt; lock the account after [`MAX_FAILS`].
    fn record_failure(&self, username: &str) {
        if let Ok(mut map) = self.attempts.lock() {
            let a = map.entry(username.to_owned()).or_default();
            a.fails += 1;
            if a.fails >= MAX_FAILS {
                a.locked_until = Some(Instant::now() + LOCKOUT);
            }
        }
    }

    /// Clear failure tracking after a successful login.
    fn clear_failures(&self, username: &str) {
        if let Ok(mut map) = self.attempts.lock() {
            map.remove(username);
        }
    }

    /// Mint an API token under `label` with `role`, recording `owner` (empty
    /// for a service token). Returns the plaintext secret exactly once, or
    /// `None` when the label is taken or persistence fails.
    fn mint_token(&self, label: &str, role: AuthRole, owner: &str) -> Option<String> {
        let mut tokens = self.tokens.lock().ok()?;
        if tokens.iter().any(|t| t.label == label) {
            return None; // label already in use
        }
        let secret = mint_token();
        tokens.push(StoredToken {
            label: label.to_owned(),
            role,
            hash: sha256_hex(&secret),
            created: now_rfc3339(),
            owner: owner.to_owned(),
        });
        drop(tokens);
        self.persist().ok()?;
        Some(secret)
    }

    /// The project memberships `owner` holds right now, or empty when the
    /// account is gone — a deleted member's personal token fails closed.
    fn owner_projects(&self, owner: &str) -> Vec<String> {
        self.users
            .lock()
            .ok()
            .and_then(|users| {
                users
                    .iter()
                    .find(|u| u.username == owner)
                    .map(|u| u.projects.clone())
            })
            .unwrap_or_default()
    }

    /// The workspace-relative default location for the user file.
    #[must_use]
    pub fn default_path(base: &Path) -> PathBuf {
        base.join("auth.json")
    }
}

/// Hash a password with Argon2id and a fresh random salt (PHC string output).
pub(crate) fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// A 256-bit random session token, hex-encoded.
pub(crate) fn mint_token() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 hex of an API token — a fast hash suitable for high-entropy secrets
/// verified on every request (unlike Argon2, which is for low-entropy passwords).
pub(crate) fn sha256_hex(token: &str) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Constant-time equality for equal-length secrets (token hashes), so matching
/// doesn't leak how many leading characters were correct via response timing.
pub(crate) fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub(crate) fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Current unix time in seconds (for TOTP).
pub(crate) fn unix_now() -> u64 {
    u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).unwrap_or(0)
}

#[async_trait]
impl AuthPort for FileAuthService {
    async fn login(&self, username: &str, password: &str, totp: Option<&str>) -> LoginResult {
        // Brute-force throttle: refuse while the account is locked.
        if self.is_locked(username) {
            return LoginResult::Denied;
        }
        // Verify the password and read back the (role, 2FA secret) atomically.
        let verified = self.users.lock().ok().and_then(|users| {
            let user = users.iter().find(|u| u.username == username)?;
            let parsed = PasswordHash::new(&user.hash).ok()?;
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .ok()?;
            Some((user.role, user.totp_secret.clone()))
        });
        let Some((role, totp_secret)) = verified else {
            self.record_failure(username);
            return LoginResult::Denied;
        };
        // Second factor when enrolled.
        if let Some(secret) = totp_secret {
            let Some(code) = totp else {
                // Password is right; the client must now supply a code.
                return LoginResult::TotpRequired;
            };
            if !crate::totp::verify(&secret, code, unix_now()) {
                self.record_failure(username);
                return LoginResult::Denied;
            }
        }
        self.clear_failures(username);
        let token = mint_token();
        let session = Session {
            user: AuthUser {
                username: username.to_owned(),
                name: String::new(),
                email: String::new(),
                role,
                projects: Vec::new(),
            },
            expires: now_unix() + SESSION_TTL.as_secs(),
            label: String::new(),
            at: now_rfc3339(),
        };
        match self.sessions.lock() {
            Ok(mut s) => {
                s.insert(token.clone(), session);
                drop(s);
                self.persist_sessions();
                LoginResult::Ok(token)
            }
            Err(_) => LoginResult::Denied,
        }
    }

    async fn user_for(&self, token: &str) -> Option<AuthUser> {
        let username = {
            let mut sessions = self.sessions.lock().ok()?;
            let session = sessions.get(token)?;
            if session.expires <= now_unix() {
                sessions.remove(token);
                drop(sessions);
                self.persist_sessions();
                return None;
            }
            session.user.username.clone()
        };
        // Resolve the user FRESH from the store on every request. The session
        // used to carry a login-time snapshot, so a role promotion or a
        // project-membership grant only took effect after the person logged
        // out and back in — an admin adds a member, the member still gets 403,
        // and both stare at a correct-looking config (found live in the hybrid
        // role-play test). Deleted users lose their sessions the same way.
        let users = self.users.lock().ok()?;
        let stored = users.iter().find(|u| u.username == username)?;
        Some(AuthUser {
            username: stored.username.clone(),
            name: stored.name.clone(),
            email: stored.email.clone(),
            role: stored.role,
            projects: stored.projects.clone(),
        })
    }

    async fn logout(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if sessions.remove(token).is_some() {
                drop(sessions);
                self.persist_sessions();
            }
        }
    }

    async fn attach_device(&self, token: &str, device: &str) {
        let changed = if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(s) = sessions.get_mut(token) {
                device.clone_into(&mut s.label);
                true
            } else {
                false
            }
        } else {
            false
        };
        if changed {
            self.persist_sessions();
        }
    }

    async fn sessions_for(&self, username: &str, current_token: &str) -> Vec<SessionInfo> {
        let Ok(sessions) = self.sessions.lock() else {
            return Vec::new();
        };
        let now = now_unix();
        let mut out: Vec<SessionInfo> = sessions
            .iter()
            .filter(|(_, s)| s.user.username == username && s.expires > now)
            .map(|(tok, s)| SessionInfo {
                label: if s.label.is_empty() {
                    "Unknown device".to_owned()
                } else {
                    s.label.clone()
                },
                at: s.at.clone(),
                current: tok == current_token,
            })
            .collect();
        out.sort_by(|a, b| b.at.cmp(&a.at));
        out
    }

    async fn principal_for_bearer(&self, token: &str) -> Option<AuthUser> {
        let hash = sha256_hex(token);
        let stored = {
            let tokens = self.tokens.lock().ok()?;
            tokens.iter().find(|t| ct_eq(&t.hash, &hash))?.clone()
        };
        // A personal token resolves its owner's project memberships LIVE —
        // never a mint-time snapshot — so unassigning a project (or deleting
        // the owner's account) revokes the token's project reach on the very
        // next call, with no re-mint. A missing owner fails closed to no
        // projects. Service tokens (owner '') stay hub-wide for their role.
        let projects = if stored.owner.is_empty() {
            Vec::new()
        } else {
            self.owner_projects(&stored.owner)
        };
        Some(AuthUser {
            username: format!("svc:{}", stored.label),
            name: String::new(),
            email: String::new(),
            role: stored.role,
            projects,
        })
    }

    async fn create_token(&self, label: &str, role: AuthRole) -> Option<String> {
        self.mint_token(label, role, "")
    }

    async fn create_token_for(
        &self,
        label: &str,
        role: AuthRole,
        owner: Option<&str>,
    ) -> Option<String> {
        self.mint_token(label, role, owner.unwrap_or_default())
    }

    async fn list_tokens(&self) -> Vec<TokenInfo> {
        self.tokens.lock().map_or_else(
            |_| Vec::new(),
            |tokens| {
                tokens
                    .iter()
                    .map(|t| TokenInfo {
                        label: t.label.clone(),
                        role: t.role,
                        created: t.created.clone(),
                    })
                    .collect()
            },
        )
    }

    async fn revoke_token(&self, label: &str) -> bool {
        let Ok(mut tokens) = self.tokens.lock() else {
            return false;
        };
        let before = tokens.len();
        tokens.retain(|t| t.label != label);
        let removed = tokens.len() != before;
        drop(tokens);
        if removed {
            let _ = self.persist();
        }
        removed
    }

    async fn list_users(&self) -> Vec<AuthUser> {
        self.users.lock().map_or_else(
            |_| Vec::new(),
            |users| {
                users
                    .iter()
                    .map(|u| AuthUser {
                        username: u.username.clone(),
                        name: u.name.clone(),
                        email: u.email.clone(),
                        role: u.role,
                        projects: u.projects.clone(),
                    })
                    .collect()
            },
        )
    }

    async fn create_user(&self, username: &str, password: &str, role: AuthRole) -> bool {
        if username.trim().is_empty() || password.is_empty() {
            return false;
        }
        let Ok(hash) = hash_password(password) else {
            return false;
        };
        {
            let Ok(mut users) = self.users.lock() else {
                return false;
            };
            match users.iter_mut().find(|u| u.username == username) {
                Some(existing) => {
                    existing.hash = hash;
                    existing.role = role;
                }
                None => users.push(StoredUser {
                    username: username.to_owned(),
                    hash,
                    name: String::new(),
                    email: String::new(),
                    role,
                    totp_secret: None,
                    projects: Vec::new(),
                }),
            }
        }
        self.persist().is_ok()
    }

    async fn update_user(
        &self,
        username: &str,
        name: &str,
        email: &str,
        role: Option<AuthRole>,
    ) -> bool {
        {
            let Ok(mut users) = self.users.lock() else {
                return false;
            };
            let Some(u) = users.iter_mut().find(|u| u.username == username) else {
                return false;
            };
            name.trim().clone_into(&mut u.name);
            email.trim().clone_into(&mut u.email);
            if let Some(r) = role {
                u.role = r;
            }
        }
        self.persist().is_ok()
    }

    async fn set_password(&self, username: &str, password: &str) -> bool {
        if password.is_empty() {
            return false;
        }
        let Ok(hash) = hash_password(password) else {
            return false;
        };
        {
            let Ok(mut users) = self.users.lock() else {
                return false;
            };
            let Some(u) = users.iter_mut().find(|u| u.username == username) else {
                return false;
            };
            u.hash = hash;
        }
        self.persist().is_ok()
    }

    async fn delete_user(&self, username: &str) -> bool {
        {
            let Ok(mut users) = self.users.lock() else {
                return false;
            };
            let exists = users.iter().any(|u| u.username == username);
            if !exists {
                return false;
            }
            // Never remove the last admin/super — that would lock everyone out.
            let is_admin_tier = |r: AuthRole| matches!(r, AuthRole::Admin | AuthRole::Super);
            let admins_left = users
                .iter()
                .filter(|u| is_admin_tier(u.role) && u.username != username)
                .count();
            let removing_admin = users
                .iter()
                .any(|u| u.username == username && is_admin_tier(u.role));
            if removing_admin && admins_left == 0 {
                return false;
            }
            users.retain(|u| u.username != username);
        }
        self.persist().is_ok()
    }

    async fn assign_project(&self, username: &str, pid: &str) -> bool {
        {
            let Ok(mut users) = self.users.lock() else {
                return false;
            };
            let Some(u) = users.iter_mut().find(|u| u.username == username) else {
                return false;
            };
            if u.projects.iter().any(|p| p == pid) {
                return true; // already a member — idempotent
            }
            u.projects.push(pid.to_owned());
        }
        self.persist().is_ok()
    }

    async fn unassign_project(&self, username: &str, pid: &str) -> bool {
        let removed = {
            let Ok(mut users) = self.users.lock() else {
                return false;
            };
            let Some(u) = users.iter_mut().find(|u| u.username == username) else {
                return false;
            };
            let before = u.projects.len();
            u.projects.retain(|p| p != pid);
            u.projects.len() != before
        };
        if removed {
            let _ = self.persist();
        }
        removed
    }

    async fn enroll_2fa(&self, username: &str) -> Option<(String, String)> {
        // The account must exist.
        if !self
            .users
            .lock()
            .ok()?
            .iter()
            .any(|u| u.username == username)
        {
            return None;
        }
        let secret = crate::totp::generate_secret();
        let uri = crate::totp::provisioning_uri(&secret, username, "CoXAgent");
        self.pending_totp
            .lock()
            .ok()?
            .insert(username.to_owned(), secret.clone());
        Some((secret, uri))
    }

    async fn enable_2fa(&self, username: &str, code: &str) -> bool {
        let Some(secret) = self
            .pending_totp
            .lock()
            .ok()
            .and_then(|m| m.get(username).cloned())
        else {
            return false;
        };
        if !crate::totp::verify(&secret, code, unix_now()) {
            return false;
        }
        {
            let Ok(mut users) = self.users.lock() else {
                return false;
            };
            let Some(user) = users.iter_mut().find(|u| u.username == username) else {
                return false;
            };
            user.totp_secret = Some(secret);
        }
        if let Ok(mut p) = self.pending_totp.lock() {
            p.remove(username);
        }
        self.persist().is_ok()
    }

    async fn disable_2fa(&self, username: &str) -> bool {
        let was_enabled = {
            let Ok(mut users) = self.users.lock() else {
                return false;
            };
            match users.iter_mut().find(|u| u.username == username) {
                Some(user) => user.totp_secret.take().is_some(),
                None => false,
            }
        };
        if was_enabled {
            let _ = self.persist();
        }
        was_enabled
    }

    async fn has_2fa(&self, username: &str) -> bool {
        self.users.lock().is_ok_and(|users| {
            users
                .iter()
                .any(|u| u.username == username && u.totp_secret.is_some())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn bootstrap_then_login_and_reject() {
        let dir = tempdir().unwrap();
        let path = FileAuthService::default_path(dir.path());
        FileAuthService::bootstrap_admin(&path, "root", "s3cret").unwrap();

        let svc = FileAuthService::open(&path).unwrap();
        assert!(svc.has_users());
        assert!(matches!(
            svc.login("root", "wrong", None).await,
            LoginResult::Denied
        ));
        let LoginResult::Ok(token) = svc.login("root", "s3cret", None).await else {
            panic!("login ok")
        };
        let user = svc.user_for(&token).await.expect("session");
        assert_eq!(user.username, "root");
        assert_eq!(user.role, AuthRole::Super);
        assert!(user.role.can_write());
        svc.logout(&token).await;
        assert!(svc.user_for(&token).await.is_none());
    }

    #[tokio::test]
    async fn re_bootstrap_updates_the_password() {
        // Regression: a stale auth.json must not lock the operator out. The
        // second bootstrap (the env password of record) wins.
        let dir = tempdir().unwrap();
        let path = FileAuthService::default_path(dir.path());
        FileAuthService::bootstrap_admin(&path, "root", "oldpass").unwrap();
        FileAuthService::bootstrap_admin(&path, "root", "newpass").unwrap();

        let svc = FileAuthService::open(&path).unwrap();
        assert!(
            matches!(
                svc.login("root", "oldpass", None).await,
                LoginResult::Denied
            ),
            "old pw revoked"
        );
        assert!(
            matches!(svc.login("root", "newpass", None).await, LoginResult::Ok(_)),
            "new pw works"
        );
        // Still exactly one account (upsert, not append).
        assert_eq!(svc.list_users().await.len(), 1);
    }

    #[tokio::test]
    async fn locks_out_after_repeated_failures() {
        let dir = tempdir().unwrap();
        let path = FileAuthService::default_path(dir.path());
        FileAuthService::bootstrap_admin(&path, "root", "s3cret").unwrap();
        let svc = FileAuthService::open(&path).unwrap();

        // Exhaust the allowed failures.
        for _ in 0..super::MAX_FAILS {
            assert!(matches!(
                svc.login("root", "wrong", None).await,
                LoginResult::Denied
            ));
        }
        // Now even the CORRECT password is refused — the account is locked.
        assert!(
            matches!(svc.login("root", "s3cret", None).await, LoginResult::Denied),
            "correct password must be refused while locked"
        );
    }

    #[tokio::test]
    async fn api_tokens_mint_authenticate_and_revoke() {
        let dir = tempdir().unwrap();
        let path = FileAuthService::default_path(dir.path());
        FileAuthService::bootstrap_admin(&path, "root", "s3cret").unwrap();
        let svc = FileAuthService::open(&path).unwrap();

        let secret = svc.create_token("ci", AuthRole::Admin).await.expect("mint");
        // Duplicate label is refused.
        assert!(svc.create_token("ci", AuthRole::Viewer).await.is_none());

        let principal = svc.principal_for_bearer(&secret).await.expect("resolve");
        assert_eq!(principal.username, "svc:ci");
        assert!(principal.role.can_write());
        assert!(svc.principal_for_bearer("nonsense").await.is_none());

        assert_eq!(svc.list_tokens().await.len(), 1);

        // Persistence: a freshly opened service sees the token, hash only.
        let reopened = FileAuthService::open(&path).unwrap();
        assert!(reopened.principal_for_bearer(&secret).await.is_some());

        assert!(svc.revoke_token("ci").await);
        assert!(!svc.revoke_token("ci").await); // already gone
        assert!(svc.principal_for_bearer(&secret).await.is_none());
    }

    #[tokio::test]
    async fn user_management_create_login_and_protect_last_admin() {
        let dir = tempdir().unwrap();
        let path = FileAuthService::default_path(dir.path());
        FileAuthService::bootstrap_admin(&path, "root", "s3cret").unwrap();
        let svc = FileAuthService::open(&path).unwrap();

        // Add a viewer; it can log in and persists.
        assert!(svc.create_user("viewer1", "pw", AuthRole::Viewer).await);
        assert!(matches!(
            svc.login("viewer1", "pw", None).await,
            LoginResult::Ok(_)
        ));
        assert_eq!(svc.list_users().await.len(), 2);
        let reopened = FileAuthService::open(&path).unwrap();
        assert!(matches!(
            reopened.login("viewer1", "pw", None).await,
            LoginResult::Ok(_)
        ));

        // The last admin cannot be deleted; a viewer can.
        assert!(!svc.delete_user("root").await, "last admin protected");
        assert!(svc.delete_user("viewer1").await);
        assert!(!svc.delete_user("nobody").await);
        assert_eq!(svc.list_users().await.len(), 1);
    }

    #[tokio::test]
    async fn totp_2fa_enroll_enable_and_gate_login() {
        let dir = tempdir().unwrap();
        let path = FileAuthService::default_path(dir.path());
        FileAuthService::bootstrap_admin(&path, "root", "s3cret").unwrap();
        let svc = FileAuthService::open(&path).unwrap();

        // Without 2FA, a plain login works.
        assert!(matches!(
            svc.login("root", "s3cret", None).await,
            LoginResult::Ok(_)
        ));

        // Enroll: a wrong confirmation code does not enable.
        let (secret, uri) = svc.enroll_2fa("root").await.expect("enroll");
        assert!(uri.starts_with("otpauth://totp/"));
        assert!(!svc.enable_2fa("root", "000000").await || !svc.has_2fa("root").await);

        // Confirm with the real code → enabled and persisted.
        let now = unix_now();
        let code = crate::totp::code_at(&secret, now).unwrap();
        assert!(svc.enable_2fa("root", &code).await);
        assert!(svc.has_2fa("root").await);

        // Now a password-only login is refused with TotpRequired…
        assert_eq!(
            svc.login("root", "s3cret", None).await,
            LoginResult::TotpRequired
        );
        // …a wrong code is Denied…
        assert_eq!(
            svc.login("root", "s3cret", Some("000000")).await,
            LoginResult::Denied
        );
        // …the correct code logs in.
        let code = crate::totp::code_at(&secret, unix_now()).unwrap();
        assert!(matches!(
            svc.login("root", "s3cret", Some(&code)).await,
            LoginResult::Ok(_)
        ));

        // Disable restores password-only login (survives reopen).
        assert!(svc.disable_2fa("root").await);
        let reopened = FileAuthService::open(&path).unwrap();
        assert!(!reopened.has_2fa("root").await);
        assert!(matches!(
            reopened.login("root", "s3cret", None).await,
            LoginResult::Ok(_)
        ));
    }

    #[tokio::test]
    async fn missing_file_is_open() {
        let dir = tempdir().unwrap();
        let svc = FileAuthService::open(&dir.path().join("nope.json")).unwrap();
        assert!(!svc.has_users());
    }

    /// CXA-F350: a member's personal token inherits the minting member's
    /// project memberships, resolved LIVE — unassignment and account
    /// deletion revoke the token's project reach on the next call with no
    /// re-mint, while a service token (owner '') grants no projects at all,
    /// even under a personal-looking label.
    #[tokio::test]
    async fn personal_tokens_inherit_their_owners_project_memberships_live() {
        let dir = tempdir().unwrap();
        let path = FileAuthService::default_path(dir.path());
        let svc = FileAuthService::open(&path).unwrap();
        assert!(svc.create_user("lead", "pw", AuthRole::TechLead).await);
        assert!(svc.assign_project("lead", "proj-a").await);
        assert!(svc.assign_project("lead", "proj-b").await);

        // Minted exactly as create_my_token_ep mints: caller as owner.
        let secret = svc
            .create_token_for("user:lead:remote-store", AuthRole::TechLead, Some("lead"))
            .await
            .expect("mint the personal token");
        let principal = svc.principal_for_bearer(&secret).await.expect("resolve");
        assert_eq!(principal.username, "svc:user:lead:remote-store");
        assert_eq!(principal.role, AuthRole::TechLead);
        assert_eq!(principal.projects, vec!["proj-a", "proj-b"]);

        // Live resolution: the same secret reflects the unassignment with no
        // re-mint.
        assert!(svc.unassign_project("lead", "proj-a").await);
        let after = svc.principal_for_bearer(&secret).await.expect("resolve");
        assert_eq!(after.projects, vec!["proj-b"]);

        // Fail closed: deleting the owner leaves the token project-less.
        assert!(svc.delete_user("lead").await);
        let orphan = svc.principal_for_bearer(&secret).await.expect("resolve");
        assert!(orphan.projects.is_empty(), "a deleted owner fails closed");
    }

    #[tokio::test]
    async fn service_tokens_stay_projectless_even_with_a_personal_looking_label() {
        let dir = tempdir().unwrap();
        let svc = FileAuthService::open(&dir.path().join("auth.json")).unwrap();
        assert!(svc.create_user("lead", "pw", AuthRole::TechLead).await);
        assert!(svc.assign_project("lead", "proj-a").await);

        // Admin-minted service token (create_token — owner '') wearing a
        // label that looks personal: the owner field is the only input, so
        // label-prefix spoofing grants nothing.
        let service = svc
            .create_token("user:lead:spoof", AuthRole::TechLead)
            .await
            .expect("mint the service token");
        let principal = svc
            .principal_for_bearer(&service)
            .await
            .expect("resolve service token");
        assert!(principal.projects.is_empty());

        // The None-owner contract of create_token_for: minting without an
        // owner is exactly a service token — no owner recorded, no projects
        // inherited.
        let unnamed = svc
            .create_token_for("user:lead:none", AuthRole::TechLead, None)
            .await
            .expect("mint the ownerless token");
        let unnamed_principal = svc
            .principal_for_bearer(&unnamed)
            .await
            .expect("resolve the ownerless token");
        assert!(
            unnamed_principal.projects.is_empty(),
            "create_token_for(None) must mint a service token"
        );
    }

    /// A pre-F350 `auth.json` (tokens without an `owner` field) loads
    /// unchanged and its tokens keep resolving — the serde default is what
    /// makes the store backward compatible.
    #[tokio::test]
    async fn a_legacy_auth_json_without_owner_fields_loads_and_resolves() {
        let dir = tempdir().unwrap();
        let path = FileAuthService::default_path(dir.path());
        let legacy = format!(
            r#"{{"users":[{{"username":"root","hash":"x","role":"admin"}}],
                "tokens":[{{"label":"ci","role":"admin","hash":"{}","created":"2026-01-01T00:00:00Z"}}]}}"#,
            sha256_hex("legacy-secret")
        );
        std::fs::write(&path, legacy).unwrap();
        let svc = FileAuthService::open(&path).unwrap();
        let principal = svc
            .principal_for_bearer("legacy-secret")
            .await
            .expect("legacy token still resolves");
        assert_eq!(principal.username, "svc:ci");
        assert!(principal.projects.is_empty());
    }
}
