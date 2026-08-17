// One logical module split across files for merge-conflict surface, not an API
// boundary — the children reach back into the parent's items wholesale, and
// enumerating ~200 shared types here would turn every rename into a two-file
// edit. Wildcard is the honest shape of that relationship.
#![allow(clippy::wildcard_imports)]
//! Sign-in, sessions, profiles and avatars — who you are to the hub.

use super::*;

/// All profiles — any signed-in user (needed to render avatars/status).
pub(super) async fn profiles_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if principal_name(&app, &headers).await.is_none() {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    }
    let doc = app.profiles.inner.lock().await;
    Json(&doc.profiles).into_response()
}

/// Update the caller's OWN status (emoji + text). Empty strings clear it.
pub(super) async fn profile_set_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ProfileReq>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let emoji: String = req.status_emoji.chars().take(8).collect();
    let text: String = req.status_text.trim().chars().take(80).collect();
    {
        let mut doc = app.profiles.inner.lock().await;
        let p = doc.profiles.entry(user.clone()).or_default();
        p.status_emoji = emoji;
        p.status_text = text;
        p.at = now_rfc3339();
    }
    app.profiles.save().await;
    audit_push(&app.audit, &user, "profile status updated".to_owned(), 200).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Clear the caller's OWN avatar, falling back to the initials tile. Upload
/// without a way back out leaves a bad photo stuck forever.
pub(super) async fn profile_avatar_clear_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    {
        let mut doc = app.profiles.inner.lock().await;
        let p = doc.profiles.entry(user.clone()).or_default();
        p.avatar.clear();
        p.at = now_rfc3339();
    }
    app.profiles.save().await;
    audit_push(&app.audit, &user, "avatar removed".to_owned(), 200).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Upload the caller's OWN avatar (image, ≤ 2 MB). Served via chat media.
pub(super) async fn profile_avatar_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    mut multipart: axum::extract::Multipart,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let Ok(Some(field)) = multipart.next_field().await else {
        return (StatusCode::BAD_REQUEST, "no file").into_response();
    };
    let data = match field.bytes().await {
        Ok(b) if b.len() <= 2 * 1024 * 1024 => b,
        Ok(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "max 2MB").into_response(),
        Err(_) => return (StatusCode::BAD_REQUEST, "read failed").into_response(),
    };
    let Some((ext, mime)) = sniff_avatar_image(&data) else {
        return (
            StatusCode::BAD_REQUEST,
            "avatar must be a real png/jpeg/gif/webp image",
        )
            .into_response();
    };
    let stored = format!("{}-avatar.{}", mint_media_token(), ext);
    if app
        .storage
        .put(&format!("chat/{stored}"), &data, mime)
        .await
        .is_err()
    {
        return internal_error("write failed");
    }
    let url = format!("/api/chat/media/{stored}");
    {
        let mut doc = app.profiles.inner.lock().await;
        let p = doc.profiles.entry(user.clone()).or_default();
        p.avatar.clone_from(&url);
        p.at = now_rfc3339();
    }
    app.profiles.save().await;
    audit_push(&app.audit, &user, "avatar updated".to_owned(), 200).await;
    Json(serde_json::json!({ "ok": true, "url": url })).into_response()
}

/// Whether `path` addresses a pull-request review action — the
/// `/api/projects/:pid/prs/:num/:action` route, whose actions are `merge`,
/// `request-changes`, `close`, `preview`, `preview-stop` and `force-merge`.
///
/// Deliberately a substring match rather than a precise route parse: over-
/// matching is safe (review rights are strictly stronger than write rights),
/// whereas under-matching would silently hand a future `/prs/` route to every
/// writer — which is exactly how COX-B038 stayed invisible.
pub(super) fn is_pr_review_path(path: &str) -> bool {
    path.contains("/prs/")
}

/// The write gate's decision, as a pure function of the caller's role and the
/// request path: PR review actions demand review rights (Super, Admin, the
/// lead tier, or the legacy Reviewer), every other write needs only ordinary
/// write rights.
///
/// Kept pure and separate from [`auth_mw`] so the policy is exhaustively
/// unit-testable per role without booting a hub or minting a session — the
/// middleware supplies role and path, and turns `false` into a 403.
pub(super) fn write_gate_ok(role: coxagent_application::auth::AuthRole, path: &str) -> bool {
    if is_pr_review_path(path) {
        return role.can_review();
    }
    // The raw store RPC (`/api/projects/:pid/store`) writes WHOLE state
    // snapshots — every ticket, every approval sample, every gate decision in
    // one POST. Ordinary write rights made it a backdoor around every role
    // gate: a member-tier account that may not approve one ticket could still
    // `op=save` a state in which the ticket was already approved. The callers
    // it exists for are remote runners, which authenticate as the operator
    // that started them — an operator account needs manage rights anyway.
    // `/api/pr-report` is its sibling: the runner reports PRs it opened, with
    // the project id in the BODY — so the per-project membership check (which
    // reads the URL) never sees it. Same caller, same bar.
    if is_store_rpc_path(path) || path == "/api/pr-report" {
        return role.can_manage();
    }
    role.can_write()
}

/// The runner store RPC: `/api/projects/<pid>/store` exactly — one path
/// segment for the pid, nothing after `store`.
pub(super) fn is_store_rpc_path(path: &str) -> bool {
    path.strip_prefix("/api/projects/")
        .and_then(|rest| rest.strip_suffix("/store"))
        .is_some_and(|pid| !pid.is_empty() && !pid.contains('/'))
}

/// RBAC gate. Open (pass-through) when no auth is configured. Otherwise: the
/// SPA shell, health, and login are public; every other route needs a valid
/// session, and mutating methods (except logout) need an admin.
#[allow(clippy::too_many_lines)] // linear gate list; splitting hides the order
pub(super) async fn auth_mw(
    State(app): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let path = req.uri().path().to_owned();
    // Physical service split: a realtime pod serves only sockets, a knowledge
    // pod serves nothing but health — enforced here, not by routing hope.
    // (Realtime endpoints carry their own guard; this blocks the REST surface.)
    let realtime_path = path.ends_with("/ws")
        || path.ends_with("/events")
        || path.ends_with("/terminal")
        || path.contains("/docs-ws");
    let role_ok = match hub_role() {
        HubRole::All => true,
        HubRole::Gateway => !realtime_path,
        HubRole::Realtime => {
            realtime_path || path == "/api/health" || path == "/api/auth/me" || path == "/"
        }
        HubRole::Knowledge => path == "/api/health",
    };
    if !role_ok {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "wrong service role for this endpoint",
        )
            .into_response();
    }
    let Some(auth) = app.auth.clone() else {
        return next.run(req).await;
    };
    // Public routes: the SPA shell, health, login, and incoming webhooks (the
    // webhook token is the credential, so no session is required).
    if path == "/"
        || path == "/api/health"
        || path == "/api/auth/login"
        // OpenAPI spec - public, like health: MCP clients and SDK generators
        // must discover endpoints without holding a hub session.
        || path == "/api/openapi.json"
        // Embedded static assets (vendored JS/CSS) — same trust level as "/".
        || path.starts_with("/assets/")
        // Installer downloads: same trust as the login page; the native
        // updater's URLSession has no web session to present.
        || path.starts_with("/api/app/download/")
        || path.starts_with("/api/chat/hook/")
        // Invite flow: the invite token IS the credential for joining.
        || path.starts_with("/join/")
        || path == "/api/workspace/join"
    {
        return next.run(req).await;
    }
    let Some(user) = resolve_principal(&auth, req.headers()).await else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthenticated" })),
        )
            .into_response();
    };
    let is_write = matches!(
        *req.method(),
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::DELETE
            | axum::http::Method::PATCH
    ) && path != "/api/auth/logout"
        && !path.starts_with("/api/auth/2fa/") // self-service, any signed-in user
        && !path.starts_with("/api/auth/my/") // personal MCP tokens: self-service
        && !path.starts_with("/api/meetings") // booking meetings: any signed-in user
        && !path.starts_with("/api/profile") // own avatar/status: self-service
        && !path.ends_with("/chat") // team chat is open to any signed-in user
        && path != "/api/chat/send" // system chat send: any signed-in user
        && path != "/api/chat/dm" // open a DM: any signed-in user
        && !path.starts_with("/api/engines/opencode") // opencode model list: any signed-in user
        && !path.contains("/channels") // create/invite channels: any signed-in user
        && !path.ends_with("/upload") // uploads are open to any signed-in user
        && path != "/api/mcp"; // MCP dispatch: role check based on JSON-RPC method, not HTTP verb
    let method = req.method().clone();
    let username = user.username.clone();
    // Management surfaces — user administration, project Settings, and API
    // tokens — are limited to Admin + lead tier (Director/Manager/*.Lead).
    // Member-tier roles (BA/FE/BE/…) can work and chat but not administer.
    let is_manage_surface = path.starts_with("/api/auth/users")
        || path.starts_with("/api/auth/tokens")
        || (path.ends_with("/config")
            && matches!(
                *req.method(),
                axum::http::Method::PUT | axum::http::Method::POST | axum::http::Method::PATCH
            ));
    if is_manage_surface && !user.role.can_manage() {
        audit_push(
            &app.audit,
            &username,
            format!("{method} {path}"),
            StatusCode::FORBIDDEN.as_u16(),
        )
        .await;
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "management role required" })),
        )
            .into_response();
    }
    // MCP dispatch requires write access, even for read-only JSON-RPC methods,
    // because the HTTP verb alone can't distinguish read from write operations.
    if path == "/api/mcp" && !user.role.can_write() {
        audit_push(&app.audit, &username, format!("{method} {path}"), 403).await;
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "insufficient role" })),
        )
            .into_response();
    }
    // Per-project access control: extract pid from URL path and verify the
    // user is assigned to that project (Super/Admin bypass, members checked).
    if let Some(pid) = extract_pid_from_path(&path) {
        let is_super_or_admin = user.role == coxagent_application::auth::AuthRole::Super
            || user.role == coxagent_application::auth::AuthRole::Admin;
        if !is_super_or_admin && !user.projects.iter().any(|p| p == pid) {
            audit_push(&app.audit, &username, format!("{method} {path}"), 403).await;
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "not a member of this project" })),
            )
                .into_response();
        }
    }
    // PR review actions (merge / request-changes / close / preview /
    // force-merge) require review rights — Admin, the lead tier, or the legacy
    // Reviewer. Every other write only needs ordinary write rights, so a
    // member-tier contributor keeps working but cannot merge their own PR.
    if is_write && !write_gate_ok(user.role, &path) {
        audit_push(
            &app.audit,
            &username,
            format!("{method} {path}"),
            StatusCode::FORBIDDEN.as_u16(),
        )
        .await;
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "insufficient role" })),
        )
            .into_response();
    }
    let resp = next.run(req).await;
    // Record every authenticated mutation with its outcome.
    if is_write {
        audit_push(
            &app.audit,
            &username,
            format!("{method} {path}"),
            resp.status().as_u16(),
        )
        .await;
    }
    resp
}

/// List user accounts (admin-only). Returns `[{username, role}]`.
pub(super) async fn list_users_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return Json(Vec::<coxagent_application::AuthUser>::new()).into_response();
    };
    let is_admin = resolve_principal(&auth, &headers)
        .await
        .is_some_and(|u| u.role.can_manage());
    if !is_admin {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    Json(auth.list_users().await).into_response()
}

/// Self-service account update: the signed-in user edits their OWN display
/// name / email / password. Never role — that stays admin-only.
pub(super) async fn self_profile_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SelfProfileReq>,
) -> axum::response::Response {
    let Some(user) = principal_name(&app, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if !auth
        .update_user(&user, req.name.trim(), req.email.trim(), None)
        .await
    {
        return (StatusCode::NOT_FOUND, "no such user").into_response();
    }
    if let Some(pw) = req.password.as_deref().filter(|p| !p.is_empty()) {
        if pw.len() < 8 {
            return (StatusCode::BAD_REQUEST, "password too short (min 8)").into_response();
        }
        if !auth.set_password(&user, pw).await {
            return (StatusCode::NOT_FOUND, "no such user").into_response();
        }
    }
    audit_push(&app.audit, &user, "own profile updated".to_owned(), 200).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Reset a user's password. Admin-only.
pub(super) async fn reset_password_ep(
    State(app): State<AppState>,
    Path(username): Path<String>,
    Json(req): Json<ResetPasswordReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    if req.password.len() < 8 {
        return (StatusCode::BAD_REQUEST, "password too short").into_response();
    }
    if auth.set_password(&username, &req.password).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such user").into_response()
    }
}

/// Begin 2FA enrollment for the signed-in user: returns the secret + otpauth URI.
pub(super) async fn enroll_2fa_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
    };
    match auth.enroll_2fa(&user.username).await {
        Some((secret, uri)) => {
            Json(serde_json::json!({ "secret": secret, "uri": uri })).into_response()
        }
        None => internal_error("could not start enrollment"),
    }
}

/// Activate 2FA for the signed-in user after confirming a code.
pub(super) async fn enable_2fa_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CodeReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
    };
    if auth.enable_2fa(&user.username, req.code.trim()).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::BAD_REQUEST, "invalid or expired code").into_response()
    }
}

/// Disable 2FA for the signed-in user.
pub(super) async fn disable_2fa_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let Some(user) = resolve_principal(&auth, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
    };
    auth.disable_2fa(&user.username).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Canonical machine-local location of the persisted remote-store bearer token.
///
/// Mirrors `coxagent_app::builders::operator_token_path` exactly so the login
/// writer (here) and the runner-side reader can never disagree about where the
/// secret lives. Env override `COXAGENT_TOKEN_FILE` wins; else
/// `<home>/CoXAgent/operator.token`. Inlined because coxagent-presentation must
/// not import from coxagent-app (dependency cycle app -> presentation).
fn operator_token_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COXAGENT_TOKEN_FILE") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    std::env::home_dir().map(|h| h.join("CoXAgent").join("operator.token"))
}

/// Persist a harvested personal API token to [`operator_token_path`], owner-only
/// (0600), so separately-spawned operator processes can read it for `/store`
/// auth. Silently no-ops when no path resolves or the write fails -- a missing
/// token file only degrades remote-store provisioning, never login itself.
fn persist_local_operator_token(secret: &str) {
    let Some(path) = operator_token_path() else {
        return;
    };

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }

    std::fs::write(&path, secret.as_bytes()).ok();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
}

pub(super) async fn login_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LoginReq>,
) -> axum::response::Response {
    use coxagent_application::LoginResult;
    let Some(auth) = app.auth.clone() else {
        return Json(serde_json::json!({ "ok": true, "auth": false })).into_response();
    };
    let token = match auth
        .login(&req.username, &req.password, req.totp.as_deref())
        .await
    {
        LoginResult::Ok(token) => {
            // Label the session with the device/browser from the User-Agent.
            let ua = headers
                .get(axum::http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            auth.attach_device(&token, &device_label(ua)).await;
            // CXA-F002: harvest a personal bearer token for remote-state runners
            // (fed into make_store's COXAGENT_REMOTE_TOKEN so /store calls are
            // authenticated under P5a). Idempotent per user — first login mints,
            // later logins reuse without re-issuing the secret.
            if let Some(secret) = auth.auto_issue_personal_token(&req.username).await {
                std::env::set_var("COXAGENT_REMOTE_TOKEN", secret.clone());
                persist_local_operator_token(&secret);
            }
            token
        }
        LoginResult::TotpRequired => {
            // Password is correct; the client must supply a 2FA code next.
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "totp required", "totp_required": true })),
            )
                .into_response();
        }
        LoginResult::Denied => {
            audit_push(
                &app.audit,
                &req.username,
                "failed login".to_owned(),
                StatusCode::UNAUTHORIZED.as_u16(),
            )
            .await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "invalid credentials" })),
            )
                .into_response();
        }
    };
    let user = auth.user_for(&token).await;
    let role = user.as_ref().map_or("viewer", |u| u.role.as_str());
    audit_push(&app.audit, &req.username, "login".to_owned(), 200).await;
    let cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200{}",
        cookie_secure(&headers)
    );
    (
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({ "ok": true, "username": req.username, "role": role })),
    )
        .into_response()
}

/// Invalidate the session and clear the cookie.
pub(super) async fn logout_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let (Some(auth), Some(token)) = (app.auth.clone(), cookie_value(&headers, SESSION_COOKIE)) {
        auth.logout(&token).await;
    }
    let cleared = format!(
        "{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{}",
        cookie_secure(&headers)
    );
    (
        [(header::SET_COOKIE, cleared)],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

#[cfg(test)]
mod operator_token_writer_tests {
    use super::persist_local_operator_token;
    use std::path::PathBuf;

    /// These tests read/write process-global env vars; serialize them so they
    /// cannot clobber one another's values when Rust runs them on many threads.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn persist_local_operator_token_writes_secret_at_env_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let base: PathBuf = dir.path().to_path_buf();
        let target = base.join("operator.token");
        // Point the canonical location at a throwaway path so we never touch a
        // real ~/CoXAgent token while testing.
        std::env::set_var("COXAGENT_TOKEN_FILE", &target);

        persist_local_operator_token("super-secret");

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "super-secret",
            "the secret should be persisted verbatim at the canonical location"
        );

        // Owner-only (0600) on unix — never a world-readable secret file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&target).unwrap();
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o600,
                "the token file must be owner-only (0600)"
            );
            assert!(meta.is_file(), "a regular file should be written");
        }

        std::env::remove_var("COXAGENT_TOKEN_FILE");
    }

    #[test]
    fn persist_local_operator_token_creates_parent_dir_and_overwrites() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        // Nest under a directory that does not exist yet.
        let target: PathBuf = dir.path().join("nested/deeply").join("operator.token");
        std::env::set_var("COXAGENT_TOKEN_FILE", &target);

        persist_local_operator_token("first");
        persist_local_operator_token("second");

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "second",
            "re-persisting should overwrite the previous secret in place"
        );

        std::env::remove_var("COXAGENT_TOKEN_FILE");
    }
}
