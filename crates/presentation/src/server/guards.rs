// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Request-admission helpers: resolve who is calling and whether they are
//! allowed to reach a given surface. Used by nearly every domain file.

use super::*;

/// Name of the session cookie.
pub(super) const SESSION_COOKIE: &str = "cox_session";

/// Which surface this process serves — the physical service split. One binary,
/// four roles (`COXAGENT_ROLE`): `all` (default, self-host single process),
/// `gateway` (REST + MCP, no sockets), `realtime` (WS/SSE only), `knowledge`
/// (batch loops only). A load balancer routes paths to the right pods; the
/// role guard makes serving the wrong surface impossible, not just unrouted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum HubRole {
    All,
    Gateway,
    Realtime,
    Knowledge,
}

pub(super) fn hub_role() -> HubRole {
    static ROLE: std::sync::OnceLock<HubRole> = std::sync::OnceLock::new();
    *ROLE.get_or_init(
        || match std::env::var("COXAGENT_ROLE").unwrap_or_default().as_str() {
            "gateway" => HubRole::Gateway,
            "realtime" => HubRole::Realtime,
            "knowledge" => HubRole::Knowledge,
            _ => HubRole::All,
        },
    )
}

/// 503 unless this process's role serves the given surface.
pub(super) fn role_guard(need_realtime: bool) -> Option<axum::response::Response> {
    let ok = match hub_role() {
        HubRole::All => true,
        HubRole::Gateway => !need_realtime,
        HubRole::Realtime => need_realtime,
        HubRole::Knowledge => false,
    };
    if ok {
        None
    } else {
        Some(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "wrong service role for this endpoint — check the load balancer routing",
            )
                .into_response(),
        )
    }
}

/// Extract a cookie value from a `Cookie` header set.
pub(super) fn cookie_value(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == name).then(|| v.trim().to_owned())
    })
}

/// Resolve the principal: an `Authorization: Bearer` API token (for automation)
/// takes precedence, else the session cookie.
pub(super) async fn resolve_principal(
    auth: &Arc<dyn AuthPort>,
    headers: &axum::http::HeaderMap,
) -> Option<coxagent_application::AuthUser> {
    if let Some(token) = bearer_token(headers) {
        if let Some(user) = auth.principal_for_bearer(&token).await {
            return Some(user);
        }
    }
    match cookie_value(headers, SESSION_COOKIE) {
        Some(token) => auth.user_for(&token).await,
        None => None,
    }
}

/// The caller's username, via session cookie or bearer token.
pub(super) async fn principal_name(
    app: &AppState,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    let Some(auth) = app.auth.clone() else {
        // Open mode (no accounts configured): every request IS the operator.
        // Returning None here made profile/meeting endpoints 401 on a hub
        // whose every other endpoint runs open — an inconsistency the e2e
        // console gate caught.
        return Some("operator".to_owned());
    };
    resolve_principal(&auth, headers).await.map(|u| u.username)
}

/// Resolve the signed-in username, or `"user"` when auth is disabled.
pub(super) async fn resolve_username(
    app: &AppState,
    headers: &axum::http::HeaderMap,
) -> String {
    match &app.auth {
        Some(auth) => resolve_principal(auth, headers)
            .await
            .map_or_else(|| "user".to_owned(), |u| u.username),
        None => "user".to_owned(),
    }
}

/// Whether the caller holds admin/super authority, which outranks channel
/// ownership everywhere it is checked.
pub(super) async fn user_can_manage(
    app: &AppState,
    headers: &axum::http::HeaderMap,
) -> bool {
    let Some(auth) = app.auth.clone() else {
        return true; // running open (no auth configured)
    };
    resolve_principal(&auth, headers)
        .await
        .is_some_and(|u| u.role.can_manage())
}

/// Resolve the caller's username and whether they may create channels.
pub(super) async fn resolve_user_caps(
    app: &AppState,
    headers: &axum::http::HeaderMap,
) -> (String, bool) {
    match &app.auth {
        Some(auth) => match resolve_principal(auth, headers).await {
            Some(u) => (u.username, u.role.can_create_channel()),
            None => ("user".to_owned(), false),
        },
        None => ("user".to_owned(), true), // open/local mode: allow
    }
}

/// Whether the caller is the hub super admin.
pub(super) async fn is_super(app: &AppState, headers: &axum::http::HeaderMap) -> bool {
    match app.auth.clone() {
        Some(auth) => resolve_principal(&auth, headers)
            .await
            .is_some_and(|u| u.role.is_super()),
        // Open mode (no auth): single-user local — allow.
        None => true,
    }
}
