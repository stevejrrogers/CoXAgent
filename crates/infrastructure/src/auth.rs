//! `FileAuthService` — the concrete [`AuthPort`]: Argon2 password hashing, a
//! JSON user file, and in-memory session tokens. The user file stores only
//! salted Argon2 hashes, never plaintext; sessions live in memory and expire.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use async_trait::async_trait;
use coxagent_application::auth::{AuthPort, AuthRole, AuthUser};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Sessions live this long before a token must be re-issued.
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// Consecutive failed logins for one account before it is locked.
const MAX_FAILS: u32 = 5;

/// How long an account stays locked after too many failures.
const LOCKOUT: Duration = Duration::from_secs(15 * 60);

/// One stored account: username, Argon2 password hash (PHC string), role.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredUser {
    username: String,
    hash: String,
    role: AuthRole,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UserFile {
    users: Vec<StoredUser>,
}

struct Session {
    user: AuthUser,
    expires: Instant,
}

/// Failed-login tracking for one account (brute-force throttle).
#[derive(Default)]
struct Attempts {
    fails: u32,
    /// When set, the account is locked until this instant.
    locked_until: Option<Instant>,
}

/// File-backed auth service with in-memory sessions.
pub struct FileAuthService {
    users: Vec<StoredUser>,
    sessions: Mutex<HashMap<String, Session>>,
    attempts: Mutex<HashMap<String, Attempts>>,
}

impl FileAuthService {
    /// Load users from `path`. A missing file yields an empty (locked) store.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be parsed.
    pub fn open(path: &Path) -> Result<Self, String> {
        let users = match std::fs::read_to_string(path) {
            Ok(text) => {
                let f: UserFile =
                    serde_json::from_str(&text).map_err(|e| format!("bad auth file: {e}"))?;
                f.users
            }
            Err(_) => Vec::new(),
        };
        Ok(Self {
            users,
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
        })
    }

    /// Ensure an admin account exists in `path`, creating the file with a hashed
    /// password if it is absent. Idempotent: an existing file is left untouched.
    ///
    /// # Errors
    /// Returns an error if hashing fails or the file cannot be written.
    pub fn bootstrap_admin(path: &Path, username: &str, password: &str) -> Result<(), String> {
        if path.exists() {
            return Ok(());
        }
        let hash = hash_password(password)?;
        let file = UserFile {
            users: vec![StoredUser {
                username: username.to_owned(),
                hash,
                role: AuthRole::Admin,
            }],
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, text).map_err(|e| e.to_string())
    }

    /// Whether any account is configured. When false the app runs open.
    #[must_use]
    pub fn has_users(&self) -> bool {
        !self.users.is_empty()
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

    /// The workspace-relative default location for the user file.
    #[must_use]
    pub fn default_path(base: &Path) -> PathBuf {
        base.join("auth.json")
    }
}

/// Hash a password with Argon2id and a fresh random salt (PHC string output).
fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// A 256-bit random session token, hex-encoded.
fn mint_token() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[async_trait]
impl AuthPort for FileAuthService {
    async fn login(&self, username: &str, password: &str) -> Option<String> {
        // Brute-force throttle: refuse while the account is locked.
        if self.is_locked(username) {
            return None;
        }
        let verified = self
            .users
            .iter()
            .find(|u| u.username == username)
            .and_then(|user| {
                let parsed = PasswordHash::new(&user.hash).ok()?;
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .ok()?;
                Some(user)
            });
        let Some(user) = verified else {
            self.record_failure(username);
            return None;
        };
        self.clear_failures(username);
        let token = mint_token();
        let session = Session {
            user: AuthUser {
                username: user.username.clone(),
                role: user.role,
            },
            expires: Instant::now() + SESSION_TTL,
        };
        self.sessions.lock().ok()?.insert(token.clone(), session);
        Some(token)
    }

    async fn user_for(&self, token: &str) -> Option<AuthUser> {
        let mut sessions = self.sessions.lock().ok()?;
        let session = sessions.get(token)?;
        if session.expires <= Instant::now() {
            sessions.remove(token);
            return None;
        }
        Some(session.user.clone())
    }

    async fn logout(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(token);
        }
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
        // Idempotent second call keeps the same file.
        FileAuthService::bootstrap_admin(&path, "root", "different").unwrap();

        let svc = FileAuthService::open(&path).unwrap();
        assert!(svc.has_users());
        assert!(svc.login("root", "wrong").await.is_none());
        let token = svc.login("root", "s3cret").await.expect("login ok");
        let user = svc.user_for(&token).await.expect("session");
        assert_eq!(user.username, "root");
        assert_eq!(user.role, AuthRole::Admin);
        assert!(user.role.can_write());
        svc.logout(&token).await;
        assert!(svc.user_for(&token).await.is_none());
    }

    #[tokio::test]
    async fn locks_out_after_repeated_failures() {
        let dir = tempdir().unwrap();
        let path = FileAuthService::default_path(dir.path());
        FileAuthService::bootstrap_admin(&path, "root", "s3cret").unwrap();
        let svc = FileAuthService::open(&path).unwrap();

        // Exhaust the allowed failures.
        for _ in 0..super::MAX_FAILS {
            assert!(svc.login("root", "wrong").await.is_none());
        }
        // Now even the CORRECT password is refused — the account is locked.
        assert!(
            svc.login("root", "s3cret").await.is_none(),
            "correct password must be refused while locked"
        );
    }

    #[tokio::test]
    async fn missing_file_is_open() {
        let dir = tempdir().unwrap();
        let svc = FileAuthService::open(&dir.path().join("nope.json")).unwrap();
        assert!(!svc.has_users());
    }
}
