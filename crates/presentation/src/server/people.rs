// Part of the server module split by concern — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! People: user accounts, tokens, project members, profiles/avatars, live
//! sessions and the people analytics the Manage tab shows.

use super::*;

/// A user's public profile bits: avatar + Slack-style status (emoji + text).
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Profile {
    #[serde(default)]
    pub(super) avatar: String,
    #[serde(default)]
    pub(super) status_emoji: String,
    #[serde(default)]
    pub(super) status_text: String,
    #[serde(default)]
    pub(super) at: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct ProfilesDoc {
    pub(super) profiles: std::collections::HashMap<String, Profile>,
}

/// Profile store: shared KV (`app_kv` key `profiles`) when configured, else a
/// local `profiles.json` under the hub dir — same shape as [`Ws`]/[`Mt`].
#[derive(Clone)]
pub(super) struct Pf {
    pub(super) inner: Arc<tokio::sync::Mutex<ProfilesDoc>>,
    pub(super) path: PathBuf,
    pub(super) store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

impl Pf {
    pub(super) async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("profiles.json");
        let text = if let Some(s) = &store {
            s.load("profiles").await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
        }
    }

    pub(super) async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save("profiles", &json).await {
                tracing::warn!("profiles save failed: {e}");
            }
            return;
        }
        let _ = std::fs::write(&self.path, json);
    }
}

#[derive(serde::Deserialize)]
pub(super) struct ProfileReq {
    #[serde(default)]
    pub(super) status_emoji: String,
    #[serde(default)]
    pub(super) status_text: String,
}

/// Identify a real raster image format from its magic bytes. Never trusts the
/// client-supplied Content-Type (trivially spoofable via multipart `type=`) —
/// SVG and every other format that can carry a `<script>` is rejected outright,
/// since there is no safe way to "sniff-validate" an XML document as inert.
pub(super) fn sniff_avatar_image(data: &[u8]) -> Option<(&'static str, &'static str)> {
    if data.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(("png", "image/png"))
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some(("gif", "image/gif"))
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else {
        None
    }
}

#[derive(serde::Deserialize)]
pub(super) struct CreateTokenReq {
    pub(super) label: String,
    /// "admin" or "viewer" (defaults to viewer).
    #[serde(default)]
    pub(super) role: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct CreateMyTokenReq {
    pub(super) label: String,
}

#[derive(serde::Deserialize)]
pub(super) struct CreateUserReq {
    pub(super) username: String,
    pub(super) password: String,
    #[serde(default)]
    pub(super) role: Option<String>,
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) email: String,
    /// Project ids to assign the new user to (a user may join many).
    #[serde(default)]
    pub(super) projects: Vec<String>,
}

pub(super) fn role_from(s: Option<&str>) -> coxagent_application::AuthRole {
    coxagent_application::AuthRole::from_str_lenient(s.unwrap_or(""))
}

/// As [`role_from`], but an UNRECOGNISED non-empty name is an error instead
/// of silently becoming Viewer — "member" quietly demoting a user to
/// read-only cost a confused hour in the hybrid role-play test.
pub(super) fn role_from_strict(
    s: Option<&str>,
) -> Result<coxagent_application::AuthRole, String> {
    use coxagent_application::AuthRole;
    let raw = s.unwrap_or("").trim();
    let role = AuthRole::from_str_lenient(raw);
    if role == AuthRole::Viewer && !raw.is_empty() && !raw.eq_ignore_ascii_case("viewer") {
        return Err(format!(
            "unknown role '{raw}' — valid: {}",
            AuthRole::all()
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(role)
}

/// Create or update a user account (admin-only via the write gate).
pub(super) async fn create_user_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateUserReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    // Only admins and leads can create user accounts.
    let is_admin = resolve_principal(&auth, &headers)
        .await
        .is_some_and(|u| u.role.can_manage());
    if !is_admin {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    if req.username.trim().is_empty() || req.password.is_empty() {
        return (StatusCode::BAD_REQUEST, "username and password required").into_response();
    }
    let username = req.username.trim();
    let role = match role_from_strict(req.role.as_deref()) {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    if !auth.create_user(username, &req.password, role).await {
        return internal_error("could not create user");
    }
    // Best-effort profile + project assignment on the freshly created account.
    if !req.name.trim().is_empty() || !req.email.trim().is_empty() {
        auth.update_user(username, req.name.trim(), req.email.trim(), None)
            .await;
    }
    for pid in &req.projects {
        auth.assign_project(username, pid).await;
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[derive(serde::Deserialize)]
pub(super) struct UpdateUserReq {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) email: String,
    #[serde(default)]
    pub(super) role: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct SelfProfileReq {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) email: String,
    #[serde(default)]
    pub(super) password: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct ResetPasswordReq {
    pub(super) password: String,
}

/// Delete a user account (admin-only). Refuses to remove the last admin.
pub(super) async fn delete_user_ep(
    State(app): State<AppState>,
    Path(username): Path<String>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if auth.delete_user(&username).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (
            StatusCode::CONFLICT,
            "cannot delete (unknown user or last admin)",
        )
            .into_response()
    }
}

#[derive(serde::Deserialize)]
pub(super) struct MemberReq {
    pub(super) username: String,
}

/// List every account with a flag for whether it's assigned to this project, so
/// the Team view can show members and offer the rest for assignment (admin-only).
pub(super) async fn list_members_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(serde_json::json!([])).into_response();
    };
    // Only admins/super can see the full member list; regular members see only
    // users assigned to their project (filtered by project membership).
    let user = resolve_principal(&auth, &headers).await;
    let Some(user) = user else {
        return (StatusCode::FORBIDDEN, "sign-in required").into_response();
    };
    let is_privileged = user.role.can_manage();
    let out: Vec<serde_json::Value> = auth
        .list_users()
        .await
        .into_iter()
        .filter(|u| is_privileged || u.projects.iter().any(|p| p.as_str() == pid.as_str()))
        .map(|u| {
            serde_json::json!({
                "username": u.username,
                "role": u.role,
                "assigned": u.projects.iter().any(|p| p == &pid),
            })
        })
        .collect();
    Json(out).into_response()
}

/// Assign a user to a project (admin-only via the write gate).
pub(super) async fn add_member_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<MemberReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if auth.assign_project(req.username.trim(), &pid).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::CONFLICT, "unknown user").into_response()
    }
}

/// Remove a user from a project (admin-only via the write gate).
pub(super) async fn remove_member_ep(
    State(app): State<AppState>,
    Path((pid, username)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if auth.unassign_project(&username, &pid).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::CONFLICT, "not a member").into_response()
    }
}

/// Running per-user aggregate used by [`people_analytics_ep`].
pub(super) struct PeopleAgg {
    pub(super) actions: u32,
    pub(super) work: u32,
    pub(super) failures: u32,
    pub(super) last_active: String,
    pub(super) days: std::collections::BTreeSet<String>,
    pub(super) by_action: std::collections::BTreeMap<String, u32>,
}

/// Per-user activity analytics for admins: who is actually working, how much,
/// how recently, and how effectively. Derived from the audit trail so it needs
/// no extra storage. Newest audit window (up to 5000 rows) is aggregated per
/// user into totals, work vs. sign-in actions, success rate, active days, and a
/// simple productivity score.
pub(super) async fn people_analytics_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Some(auth) = app.auth.clone() {
        let ok = match resolve_principal(&auth, &headers).await {
            Some(u) => u.role.can_write(),
            None => false,
        };
        if !ok {
            return (StatusCode::FORBIDDEN, "admin role required").into_response();
        }
    }
    let entries = match app.audit.recent(5000).await {
        Ok(e) => e,
        Err(e) => return internal_error(&e.to_string()),
    };

    let mut per: HashMap<String, PeopleAgg> = HashMap::new();
    for e in &entries {
        let a = per.entry(e.user.clone()).or_insert_with(|| PeopleAgg {
            actions: 0,
            work: 0,
            failures: 0,
            last_active: String::new(),
            days: std::collections::BTreeSet::new(),
            by_action: std::collections::BTreeMap::new(),
        });
        a.actions += 1;
        // "Work" = anything that changes state, i.e. not a passive sign-in/read.
        let sign_in = matches!(e.action.as_str(), "login" | "logout" | "2fa-verify");
        if !sign_in {
            a.work += 1;
        }
        if e.status >= 400 {
            a.failures += 1;
        }
        if e.at > a.last_active {
            a.last_active.clone_from(&e.at);
        }
        if let Some(day) = e.at.split('T').next() {
            a.days.insert(day.to_owned());
        }
        *a.by_action.entry(e.action.clone()).or_insert(0) += 1;
    }

    let mut people: Vec<serde_json::Value> = per
        .into_iter()
        .map(|(user, a)| {
            let success = if a.actions == 0 {
                100.0
            } else {
                f64::from(a.actions - a.failures) / f64::from(a.actions) * 100.0
            };
            // Top actions, most frequent first.
            let mut top: Vec<(String, u32)> = a.by_action.into_iter().collect();
            top.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
            top.truncate(4);
            serde_json::json!({
                "user": user,
                "actions": a.actions,
                "work": a.work,
                "success_rate": (success * 10.0).round() / 10.0,
                "active_days": a.days.len(),
                "last_active": a.last_active,
                "top_actions": top.iter().map(|(k,v)| serde_json::json!({"action":k,"count":v})).collect::<Vec<_>>(),
            })
        })
        .collect();
    // Busiest workers first.
    people.sort_by_key(|p| {
        std::cmp::Reverse(
            p.get("work")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        )
    });
    Json(serde_json::json!({ "people": people, "sample": entries.len() })).into_response()
}

pub(super) fn device_label(ua: &str) -> String {
    if ua.trim().is_empty() {
        return "Unknown device".to_owned();
    }
    let browser = if ua.contains("Edg") {
        "Edge"
    } else if ua.contains("OPR") || ua.contains("Opera") {
        "Opera"
    } else if ua.contains("Chrome") {
        "Chrome"
    } else if ua.contains("Firefox") {
        "Firefox"
    } else if ua.contains("Safari") {
        "Safari"
    } else {
        "Browser"
    };
    let os = if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("iPhone") {
        "iPhone"
    } else if ua.contains("iPad") {
        "iPad"
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        "device"
    };
    format!("{browser} on {os}")
}

/// The signed-in user's active sessions (where they're logged in), for the
/// "your devices" view. The caller's own session is flagged `current`.
pub(super) async fn sessions_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(serde_json::json!([])).into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
    };
    let token = cookie_value(&headers, SESSION_COOKIE).unwrap_or_default();
    Json(auth.sessions_for(&user.username, &token).await).into_response()
}

/// The `; Secure` cookie attribute when the connection is TLS-terminated —
/// detected via `X-Forwarded-Proto: https` (behind a reverse proxy) or the
/// `COXAGENT_SECURE_COOKIES=1` opt-in. Omitted for plain-HTTP localhost so the
/// cookie still works there.
pub(super) fn cookie_secure(headers: &axum::http::HeaderMap) -> &'static str {
    let forwarded_https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p.eq_ignore_ascii_case("https"));
    let forced = std::env::var("COXAGENT_SECURE_COOKIES")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    if forwarded_https || forced {
        "; Secure"
    } else {
        ""
    }
}

/// Report the current principal (or `auth:false` when running open).
pub(super) async fn me_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(serde_json::json!({ "auth": false })).into_response();
    };
    match resolve_principal(&auth, &headers).await {
        Some(u) => {
            let twofa = auth.has_2fa(&u.username).await;
            // Session principals are login-time snapshots; read the CURRENT
            // record so a just-saved display name/email shows immediately.
            let fresh = auth
                .list_users()
                .await
                .into_iter()
                .find(|x| x.username == u.username);
            let (name, email) =
                fresh.map_or((u.name.clone(), u.email.clone()), |f| (f.name, f.email));
            Json(serde_json::json!({
                "auth": true, "username": u.username,
                "name": name, "email": email,
                "role": u.role.as_str(),
                "twofa": twofa,
            }))
            .into_response()
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "auth": true })),
        )
            .into_response(),
    }
}
