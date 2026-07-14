//! `SqlAuthService` — a Postgres-backed [`AuthPort`]. Accounts, project
//! membership, API tokens, and 2FA secrets live in the shared database so a
//! team's collaborative auth data is server-side (not a local `auth.json`).
//! Sessions and brute-force counters stay in memory (per-instance, ephemeral).

use argon2::password_hash::{PasswordHash, PasswordVerifier};
use argon2::Argon2;
use async_trait::async_trait;
use coxagent_application::auth::{AuthPort, AuthRole, AuthUser, LoginResult, TokenInfo};
use deadpool_postgres::{Config, Pool, Runtime};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_postgres::NoTls;

use crate::auth::{hash_password, mint_token, now_rfc3339, sha256_hex, unix_now};

const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_FAILS: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(15 * 60);

const INIT_SQL: &str = "
CREATE TABLE IF NOT EXISTS auth_users (
    username    TEXT PRIMARY KEY,
    hash        TEXT NOT NULL,
    role        TEXT NOT NULL,
    totp_secret TEXT,
    projects    JSONB NOT NULL DEFAULT '[]'
);
CREATE TABLE IF NOT EXISTS auth_tokens (
    label   TEXT PRIMARY KEY,
    role    TEXT NOT NULL,
    hash    TEXT NOT NULL,
    created TEXT NOT NULL
);";

struct Session {
    user: AuthUser,
    expires: Instant,
}

#[derive(Default)]
struct Attempts {
    fails: u32,
    locked_until: Option<Instant>,
}

/// Postgres-backed auth service with in-memory sessions.
pub struct SqlAuthService {
    pool: Pool,
    sessions: Mutex<HashMap<String, Session>>,
    attempts: Mutex<HashMap<String, Attempts>>,
    /// Pending (unconfirmed) 2FA secrets, keyed by username, until `enable_2fa`.
    pending_2fa: Mutex<HashMap<String, String>>,
}

fn role_str(role: AuthRole) -> &'static str {
    match role {
        AuthRole::Admin => "admin",
        AuthRole::Viewer => "viewer",
    }
}

fn role_from(s: &str) -> AuthRole {
    if s == "admin" {
        AuthRole::Admin
    } else {
        AuthRole::Viewer
    }
}

impl SqlAuthService {
    /// Connect to `dsn` and ensure the auth tables exist.
    ///
    /// # Errors
    /// Returns a message if the pool cannot be built or migration fails.
    pub async fn connect(dsn: &str) -> Result<Self, String> {
        let mut cfg = Config::new();
        cfg.url = Some(dsn.to_owned());
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| format!("auth pool: {e}"))?;
        let svc = Self {
            pool,
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            pending_2fa: Mutex::new(HashMap::new()),
        };
        svc.client()
            .await?
            .batch_execute(INIT_SQL)
            .await
            .map_err(|e| format!("auth migrate: {e}"))?;
        Ok(svc)
    }

    /// UPSERT an admin from the environment (env is authoritative each launch).
    ///
    /// # Errors
    /// Returns a message if hashing or the write fails.
    pub async fn bootstrap_admin(&self, username: &str, password: &str) -> Result<(), String> {
        let hash = hash_password(password)?;
        self.client()
            .await?
            .execute(
                "INSERT INTO auth_users (username, hash, role) VALUES ($1, $2, 'admin')
                 ON CONFLICT (username) DO UPDATE SET hash = EXCLUDED.hash, role = 'admin'",
                &[&username, &hash],
            )
            .await
            .map(|_| ())
            .map_err(|e| format!("bootstrap admin: {e}"))
    }

    async fn client(&self) -> Result<deadpool_postgres::Client, String> {
        self.pool
            .get()
            .await
            .map_err(|e| format!("connection: {e}"))
    }

    fn is_locked(&self, username: &str) -> bool {
        self.attempts.lock().is_ok_and(|a| {
            a.get(username)
                .and_then(|x| x.locked_until)
                .is_some_and(|t| t > Instant::now())
        })
    }

    fn record_failure(&self, username: &str) {
        if let Ok(mut a) = self.attempts.lock() {
            let e = a.entry(username.to_owned()).or_default();
            e.fails += 1;
            if e.fails >= MAX_FAILS {
                e.locked_until = Some(Instant::now() + LOCKOUT);
                e.fails = 0;
            }
        }
    }

    fn clear_failures(&self, username: &str) {
        if let Ok(mut a) = self.attempts.lock() {
            a.remove(username);
        }
    }
}

#[async_trait]
impl AuthPort for SqlAuthService {
    async fn login(&self, username: &str, password: &str, totp: Option<&str>) -> LoginResult {
        if self.is_locked(username) {
            return LoginResult::Denied;
        }
        let Ok(client) = self.client().await else {
            return LoginResult::Denied;
        };
        let row = client
            .query_opt(
                "SELECT hash, role, totp_secret FROM auth_users WHERE username = $1",
                &[&username],
            )
            .await
            .ok()
            .flatten();
        let Some(row) = row else {
            self.record_failure(username);
            return LoginResult::Denied;
        };
        let hash: String = row.get(0);
        let role = role_from(&row.get::<_, String>(1));
        let totp_secret: Option<String> = row.get(2);

        let verified = PasswordHash::new(&hash)
            .ok()
            .and_then(|p| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &p)
                    .ok()
            })
            .is_some();
        if !verified {
            self.record_failure(username);
            return LoginResult::Denied;
        }
        if let Some(secret) = totp_secret {
            let Some(code) = totp else {
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
                role,
                projects: Vec::new(),
            },
            expires: Instant::now() + SESSION_TTL,
        };
        match self.sessions.lock() {
            Ok(mut s) => {
                s.insert(token.clone(), session);
                LoginResult::Ok(token)
            }
            Err(_) => LoginResult::Denied,
        }
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
        if let Ok(mut s) = self.sessions.lock() {
            s.remove(token);
        }
    }

    async fn principal_for_bearer(&self, token: &str) -> Option<AuthUser> {
        let hash = sha256_hex(token);
        let client = self.client().await.ok()?;
        let row = client
            .query_opt(
                "SELECT label, role FROM auth_tokens WHERE hash = $1",
                &[&hash],
            )
            .await
            .ok()
            .flatten()?;
        Some(AuthUser {
            username: format!("svc:{}", row.get::<_, String>(0)),
            role: role_from(&row.get::<_, String>(1)),
            projects: Vec::new(),
        })
    }

    async fn create_token(&self, label: &str, role: AuthRole) -> Option<String> {
        let client = self.client().await.ok()?;
        if client
            .query_opt("SELECT 1 FROM auth_tokens WHERE label = $1", &[&label])
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return None; // label taken
        }
        let secret = mint_token();
        client
            .execute(
                "INSERT INTO auth_tokens (label, role, hash, created) VALUES ($1, $2, $3, $4)",
                &[
                    &label,
                    &role_str(role),
                    &sha256_hex(&secret),
                    &now_rfc3339(),
                ],
            )
            .await
            .ok()?;
        Some(secret)
    }

    async fn list_tokens(&self) -> Vec<TokenInfo> {
        let Ok(client) = self.client().await else {
            return Vec::new();
        };
        client
            .query(
                "SELECT label, role, created FROM auth_tokens ORDER BY created DESC",
                &[],
            )
            .await
            .map(|rows| {
                rows.iter()
                    .map(|r| TokenInfo {
                        label: r.get(0),
                        role: role_from(&r.get::<_, String>(1)),
                        created: r.get(2),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn revoke_token(&self, label: &str) -> bool {
        let Ok(client) = self.client().await else {
            return false;
        };
        client
            .execute("DELETE FROM auth_tokens WHERE label = $1", &[&label])
            .await
            .is_ok_and(|n| n > 0)
    }

    async fn list_users(&self) -> Vec<AuthUser> {
        let Ok(client) = self.client().await else {
            return Vec::new();
        };
        client
            .query(
                "SELECT username, role, projects FROM auth_users ORDER BY username",
                &[],
            )
            .await
            .map(|rows| {
                rows.iter()
                    .map(|r| AuthUser {
                        username: r.get(0),
                        role: role_from(&r.get::<_, String>(1)),
                        projects: serde_json::from_value(r.get(2)).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn create_user(&self, username: &str, password: &str, role: AuthRole) -> bool {
        if username.trim().is_empty() || password.is_empty() {
            return false;
        }
        let Ok(hash) = hash_password(password) else {
            return false;
        };
        let Ok(client) = self.client().await else {
            return false;
        };
        client
            .execute(
                "INSERT INTO auth_users (username, hash, role) VALUES ($1, $2, $3)
                 ON CONFLICT (username) DO UPDATE SET hash = EXCLUDED.hash, role = EXCLUDED.role",
                &[&username, &hash, &role_str(role)],
            )
            .await
            .is_ok()
    }

    async fn delete_user(&self, username: &str) -> bool {
        let Ok(client) = self.client().await else {
            return false;
        };
        // Never remove the last admin.
        let admins_left: i64 = client
            .query_one(
                "SELECT count(*) FROM auth_users WHERE role = 'admin' AND username <> $1",
                &[&username],
            )
            .await
            .map_or(0, |r| r.get(0));
        let removing_admin = client
            .query_opt(
                "SELECT 1 FROM auth_users WHERE username = $1 AND role = 'admin'",
                &[&username],
            )
            .await
            .ok()
            .flatten()
            .is_some();
        if removing_admin && admins_left == 0 {
            return false;
        }
        client
            .execute("DELETE FROM auth_users WHERE username = $1", &[&username])
            .await
            .is_ok_and(|n| n > 0)
    }

    async fn assign_project(&self, username: &str, pid: &str) -> bool {
        let Ok(client) = self.client().await else {
            return false;
        };
        // Append pid to the projects array if not already present (idempotent).
        client
            .execute(
                "UPDATE auth_users
                 SET projects = projects || to_jsonb($2::text)
                 WHERE username = $1 AND NOT projects @> to_jsonb($2::text)",
                &[&username, &pid],
            )
            .await
            .is_ok()
            && client
                .query_opt("SELECT 1 FROM auth_users WHERE username = $1", &[&username])
                .await
                .ok()
                .flatten()
                .is_some()
    }

    async fn unassign_project(&self, username: &str, pid: &str) -> bool {
        let Ok(client) = self.client().await else {
            return false;
        };
        client
            .execute(
                "UPDATE auth_users
                 SET projects = projects - $2
                 WHERE username = $1 AND projects @> to_jsonb($2::text)",
                &[&username, &pid],
            )
            .await
            .is_ok_and(|n| n > 0)
    }

    async fn enroll_2fa(&self, username: &str) -> Option<(String, String)> {
        let client = self.client().await.ok()?;
        client
            .query_opt("SELECT 1 FROM auth_users WHERE username = $1", &[&username])
            .await
            .ok()
            .flatten()?;
        let secret = crate::totp::generate_secret();
        let uri = crate::totp::provisioning_uri(&secret, username, "CoXAgent");
        if let Ok(mut p) = self.pending_2fa.lock() {
            p.insert(username.to_owned(), secret.clone());
        }
        Some((secret, uri))
    }

    async fn enable_2fa(&self, username: &str, code: &str) -> bool {
        let secret = match self.pending_2fa.lock() {
            Ok(p) => p.get(username).cloned(),
            Err(_) => None,
        };
        let Some(secret) = secret else {
            return false;
        };
        if !crate::totp::verify(&secret, code, unix_now()) {
            return false;
        }
        let Ok(client) = self.client().await else {
            return false;
        };
        let ok = client
            .execute(
                "UPDATE auth_users SET totp_secret = $2 WHERE username = $1",
                &[&username, &secret],
            )
            .await
            .is_ok_and(|n| n > 0);
        if ok {
            if let Ok(mut p) = self.pending_2fa.lock() {
                p.remove(username);
            }
        }
        ok
    }

    async fn disable_2fa(&self, username: &str) -> bool {
        let Ok(client) = self.client().await else {
            return false;
        };
        client
            .execute(
                "UPDATE auth_users SET totp_secret = NULL WHERE username = $1 AND totp_secret IS NOT NULL",
                &[&username],
            )
            .await
            .is_ok_and(|n| n > 0)
    }

    async fn has_2fa(&self, username: &str) -> bool {
        let Ok(client) = self.client().await else {
            return false;
        };
        client
            .query_opt(
                "SELECT 1 FROM auth_users WHERE username = $1 AND totp_secret IS NOT NULL",
                &[&username],
            )
            .await
            .ok()
            .flatten()
            .is_some()
    }
}
