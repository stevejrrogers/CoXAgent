//! `SqlAuthService` — a Postgres-backed [`AuthPort`]. Accounts, project
//! membership, API tokens, 2FA secrets, and login sessions live in the shared
//! database so a team's collaborative auth data is server-side (not a local
//! `auth.json`). Sessions are persisted (with a wall-clock expiry) and restored
//! on connect, so a hub restart never signs anyone out. Brute-force counters
//! stay in memory (per-instance, ephemeral — a lockout resetting on restart is
//! harmless).

use argon2::password_hash::{PasswordHash, PasswordVerifier};
use argon2::Argon2;
use async_trait::async_trait;
use coxagent_application::auth::{
    AuthPort, AuthRole, AuthUser, LoginResult, SessionInfo, TokenInfo,
};
use deadpool_postgres::{Config, Pool, Runtime};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_postgres::NoTls;

use crate::auth::{hash_password, mint_token, now_rfc3339, sha256_hex, unix_now};

const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const MAX_FAILS: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(15 * 60);

const INIT_SQL: &str = "
CREATE TABLE IF NOT EXISTS auth_users (
    username    TEXT PRIMARY KEY,
    hash        TEXT NOT NULL,
    role        TEXT NOT NULL,
    totp_secret TEXT,
    projects    JSONB NOT NULL DEFAULT '[]',
    name        TEXT NOT NULL DEFAULT '',
    email       TEXT NOT NULL DEFAULT ''
);
ALTER TABLE auth_users ADD COLUMN IF NOT EXISTS name  TEXT NOT NULL DEFAULT '';
ALTER TABLE auth_users ADD COLUMN IF NOT EXISTS email TEXT NOT NULL DEFAULT '';
CREATE TABLE IF NOT EXISTS auth_tokens (
    label   TEXT PRIMARY KEY,
    role    TEXT NOT NULL,
    hash    TEXT NOT NULL,
    created TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS auth_sessions (
    token    TEXT PRIMARY KEY,
    username TEXT NOT NULL,
    role     TEXT NOT NULL,
    expires  BIGINT NOT NULL,
    label    TEXT NOT NULL DEFAULT '',
    at       TEXT NOT NULL DEFAULT ''
);";

struct Session {
    user: AuthUser,
    expires: Instant,
    label: String,
    at: String,
}

/// The durable session record when sessions live in Redis (native-TTL keys).
#[derive(serde::Serialize, serde::Deserialize)]
struct SessionRec {
    username: String,
    role: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    at: String,
    /// Wall-clock expiry (unix secs) — lets a restart rebuild the deadline even
    /// though Redis also auto-expires the key.
    expires: u64,
}

/// Redis key for one session token.
fn session_key(token: &str) -> String {
    format!("cox:auth:session:{token}")
}

#[derive(Default)]
struct Attempts {
    fails: u32,
    locked_until: Option<Instant>,
}

/// Postgres-backed auth service with in-memory sessions.
pub struct SqlAuthService {
    pool: Pool,
    /// Fast in-memory cache of live sessions; the durable copy is Redis (native
    /// TTL) when configured, else the `auth_sessions` Postgres table.
    sessions: Mutex<HashMap<String, Session>>,
    /// When set, sessions live in Redis as `cox:auth:session:*` TTL keys.
    redis: Option<redis::Client>,
    attempts: Mutex<HashMap<String, Attempts>>,
    /// Pending (unconfirmed) 2FA secrets, keyed by username, until `enable_2fa`.
    pending_2fa: Mutex<HashMap<String, String>>,
}

fn role_str(role: AuthRole) -> &'static str {
    role.as_str()
}

fn role_from(s: &str) -> AuthRole {
    AuthRole::from_str_lenient(s)
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
            redis: None,
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

    /// Route session storage through Redis (native-TTL keys) instead of the
    /// Postgres `auth_sessions` table. Call before serving so restored sessions
    /// come from Redis. The first command establishes the connection (lazy).
    ///
    /// # Errors
    /// Returns a message if the Redis URL is invalid.
    pub fn with_redis(mut self, url: &str) -> Result<Self, String> {
        self.redis = Some(redis::Client::open(url).map_err(|e| format!("auth redis open: {e}"))?);
        Ok(self)
    }

    /// Restore live sessions into the in-memory cache. Must be called after any
    /// `with_redis`, so the correct backend is queried.
    pub async fn restore_sessions(&self) {
        self.load_sessions().await;
    }

    async fn redis_conn(&self) -> Option<redis::aio::MultiplexedConnection> {
        self.redis
            .as_ref()?
            .get_multiplexed_async_connection()
            .await
            .ok()
    }

    /// Rebuild one cache entry from a durable record's fields.
    fn cache_session(
        &self,
        token: String,
        username: String,
        role: AuthRole,
        label: String,
        at: String,
        remaining: u64,
    ) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(
                token,
                Session {
                    user: AuthUser {
                        username,
                        name: String::new(),
                        email: String::new(),
                        role,
                        projects: Vec::new(),
                    },
                    expires: Instant::now() + Duration::from_secs(remaining),
                    label,
                    at,
                },
            );
        }
    }

    /// Restore non-expired sessions into the in-memory cache so a hub restart
    /// keeps everyone logged in. Reads from Redis when configured, else Postgres.
    async fn load_sessions(&self) {
        if self.redis.is_some() {
            self.load_sessions_redis().await;
            return;
        }
        let Ok(client) = self.client().await else {
            return;
        };
        let now = unix_now();
        let now_i = i64::try_from(now).unwrap_or(i64::MAX);
        let _ = client
            .execute("DELETE FROM auth_sessions WHERE expires <= $1", &[&now_i])
            .await;
        let Ok(rows) = client
            .query(
                "SELECT token, username, role, expires, label, at FROM auth_sessions WHERE expires > $1",
                &[&now_i],
            )
            .await
        else {
            return;
        };
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        for row in rows {
            let token: String = row.get(0);
            let username: String = row.get(1);
            let role = role_from(&row.get::<_, String>(2));
            let expires_unix: i64 = row.get(3);
            let label: String = row.get(4);
            let at: String = row.get(5);
            // Rebuild the monotonic deadline from the wall-clock expiry.
            let remaining = u64::try_from((expires_unix - now_i).max(0)).unwrap_or(0);
            sessions.insert(
                token,
                Session {
                    user: AuthUser {
                        username,
                        name: String::new(),
                        email: String::new(),
                        role,
                        projects: Vec::new(),
                    },
                    expires: Instant::now() + Duration::from_secs(remaining),
                    label,
                    at,
                },
            );
        }
        tracing::info!("auth: restored {} session(s) from Postgres", sessions.len());
    }

    /// Restore live sessions from Redis (`cox:auth:session:*`). Redis's native
    /// TTL means only non-expired keys still exist — no manual pruning.
    async fn load_sessions_redis(&self) {
        let Some(mut c) = self.redis_conn().await else {
            return;
        };
        let mut keys: Vec<String> = Vec::new();
        let mut cursor = 0u64;
        loop {
            let Ok((next, batch)) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg("cox:auth:session:*")
                .arg("COUNT")
                .arg(200)
                .query_async::<(u64, Vec<String>)>(&mut c)
                .await
            else {
                return;
            };
            keys.extend(batch);
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        if keys.is_empty() {
            tracing::info!("auth: restored 0 session(s) from Redis");
            return;
        }
        let Ok(vals) = redis::cmd("MGET")
            .arg(&keys)
            .query_async::<Vec<Option<String>>>(&mut c)
            .await
        else {
            return;
        };
        let now = unix_now();
        let mut restored = 0usize;
        for (key, val) in keys.iter().zip(vals) {
            let Some(rec) = val.and_then(|s| serde_json::from_str::<SessionRec>(&s).ok()) else {
                continue;
            };
            let token = key.rsplit(':').next().unwrap_or(key).to_owned();
            let remaining = rec.expires.saturating_sub(now);
            self.cache_session(
                token,
                rec.username,
                role_from(&rec.role),
                rec.label,
                rec.at,
                remaining,
            );
            restored += 1;
        }
        tracing::info!("auth: restored {restored} session(s) from Redis");
    }

    /// Best-effort write-through of one session to the durable store.
    async fn persist_session(&self, token: &str, username: &str, role: AuthRole, at: &str) {
        let expires = unix_now() + SESSION_TTL.as_secs();
        if let Some(mut c) = self.redis_conn().await {
            let rec = SessionRec {
                username: username.to_owned(),
                role: role_str(role).to_owned(),
                label: String::new(),
                at: at.to_owned(),
                expires,
            };
            let ttl_ms = u64::try_from(SESSION_TTL.as_millis()).unwrap_or(u64::MAX);
            let _ = redis::cmd("SET")
                .arg(session_key(token))
                .arg(serde_json::to_string(&rec).unwrap_or_default())
                .arg("PX")
                .arg(ttl_ms)
                .query_async::<()>(&mut c)
                .await;
            return;
        }
        let Ok(client) = self.client().await else {
            return;
        };
        let expires = i64::try_from(expires).unwrap_or(i64::MAX);
        let _ = client
            .execute(
                "INSERT INTO auth_sessions (token, username, role, expires, label, at)
                 VALUES ($1, $2, $3, $4, '', $5)
                 ON CONFLICT (token) DO UPDATE SET expires = EXCLUDED.expires",
                &[&token, &username, &role_str(role), &expires, &at],
            )
            .await;
    }

    /// Best-effort delete of one session from the durable store.
    async fn forget_session(&self, token: &str) {
        if let Some(mut c) = self.redis_conn().await {
            let _ = redis::cmd("DEL")
                .arg(session_key(token))
                .query_async::<()>(&mut c)
                .await;
            return;
        }
        if let Ok(client) = self.client().await {
            let _ = client
                .execute("DELETE FROM auth_sessions WHERE token = $1", &[&token])
                .await;
        }
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
                "INSERT INTO auth_users (username, hash, role) VALUES ($1, $2, 'super')
                 ON CONFLICT (username) DO UPDATE SET hash = EXCLUDED.hash, role = 'super'",
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
        let at = now_rfc3339();
        // Persist first so a crash right after login still leaves a valid,
        // restorable session — then cache it in memory for fast validation.
        self.persist_session(&token, username, role, &at).await;
        let session = Session {
            user: AuthUser {
                username: username.to_owned(),
                name: String::new(),
                email: String::new(),
                role,
                projects: Vec::new(),
            },
            expires: Instant::now() + SESSION_TTL,
            label: String::new(),
            at,
        };
        match self.sessions.lock() {
            Ok(mut s) => {
                s.insert(token.clone(), session);
                LoginResult::Ok(token)
            }
            Err(_) => LoginResult::Denied,
        }
    }

    async fn attach_device(&self, token: &str, device: &str) {
        if let Ok(mut s) = self.sessions.lock() {
            if let Some(sess) = s.get_mut(token) {
                device.clone_into(&mut sess.label);
            }
        }
        if let Some(mut c) = self.redis_conn().await {
            // Read-modify-write the label, keeping the remaining TTL.
            if let Ok(Some(js)) = redis::cmd("GET")
                .arg(session_key(token))
                .query_async::<Option<String>>(&mut c)
                .await
            {
                if let Ok(mut rec) = serde_json::from_str::<SessionRec>(&js) {
                    device.clone_into(&mut rec.label);
                    if let Ok(v) = serde_json::to_string(&rec) {
                        let _ = redis::cmd("SET")
                            .arg(session_key(token))
                            .arg(v)
                            .arg("KEEPTTL")
                            .query_async::<()>(&mut c)
                            .await;
                    }
                }
            }
            return;
        }
        if let Ok(client) = self.client().await {
            let _ = client
                .execute(
                    "UPDATE auth_sessions SET label = $1 WHERE token = $2",
                    &[&device, &token],
                )
                .await;
        }
    }

    async fn sessions_for(&self, username: &str, current_token: &str) -> Vec<SessionInfo> {
        let Ok(sessions) = self.sessions.lock() else {
            return Vec::new();
        };
        let now = Instant::now();
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
        self.forget_session(token).await;
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
            name: String::new(),
            email: String::new(),
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
                "SELECT username, role, projects, name, email FROM auth_users ORDER BY username",
                &[],
            )
            .await
            .map(|rows| {
                rows.iter()
                    .map(|r| AuthUser {
                        username: r.get(0),
                        role: role_from(&r.get::<_, String>(1)),
                        projects: serde_json::from_value(r.get(2)).unwrap_or_default(),
                        name: r.get(3),
                        email: r.get(4),
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

    async fn update_user(
        &self,
        username: &str,
        name: &str,
        email: &str,
        role: Option<AuthRole>,
    ) -> bool {
        let Ok(client) = self.client().await else {
            return false;
        };
        let name = name.trim();
        let email = email.trim();
        let n = match role {
            Some(r) => client
                .execute(
                    "UPDATE auth_users SET name = $2, email = $3, role = $4 WHERE username = $1",
                    &[&username, &name, &email, &role_str(r)],
                )
                .await,
            None => {
                client
                    .execute(
                        "UPDATE auth_users SET name = $2, email = $3 WHERE username = $1",
                        &[&username, &name, &email],
                    )
                    .await
            }
        };
        n.is_ok_and(|rows| rows > 0)
    }

    async fn set_password(&self, username: &str, password: &str) -> bool {
        if password.is_empty() {
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
                "UPDATE auth_users SET hash = $2 WHERE username = $1",
                &[&username, &hash],
            )
            .await
            .is_ok_and(|rows| rows > 0)
    }

    async fn delete_user(&self, username: &str) -> bool {
        let Ok(client) = self.client().await else {
            return false;
        };
        // Never remove the last admin.
        let admins_left: i64 = client
            .query_one(
                "SELECT count(*) FROM auth_users WHERE role IN ('admin','super') AND username <> $1",
                &[&username],
            )
            .await
            .map_or(0, |r| r.get(0));
        let removing_admin = client
            .query_opt(
                "SELECT 1 FROM auth_users WHERE username = $1 AND role IN ('admin','super')",
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
