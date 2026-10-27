//! SSO/OIDC browser-flow routes (slice 3).
//!
//! `GET /api/auth/oidc/login` starts an authorization-code + PKCE flow and
//! redirects to the IdP; `GET /api/auth/oidc/callback` (slice 3b) completes it.
//! Both are public (the user has no hub session yet) and GATED: with no
//! `COXAGENT_OIDC_*` configured they 404, so the live password-only deployment
//! is unchanged until an operator turns SSO on.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use coxagent_application::oidc::{role_from_groups, OidcConfig};
use coxagent_infrastructure::oidc_auth::{
    authorize_url, fetch_endpoints, fetch_jwks, pkce_challenge, random_token, verify_id_token,
};

use super::guards::SESSION_COOKIE;
use super::manage::audit_push;
use super::people::cookie_secure;
use super::AppState;

/// `state → (pkce_verifier, nonce, created_unix)` for in-flight logins.
/// Process-global in-memory: OIDC flows are short-lived and this is a
/// single-hub deployment, so a restart merely makes an in-flight login retry.
type Flows = Mutex<HashMap<String, (String, String, u64)>>;

pub(super) fn oidc_flows() -> &'static Flows {
    static FLOWS: OnceLock<Flows> = OnceLock::new();
    FLOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// How long an unfinished login may sit before its state is pruned.
pub(super) const FLOW_TTL_SECS: u64 = 600;

pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Start an OIDC login: mint state/nonce/PKCE, stash them, redirect to the IdP.
pub(super) async fn oidc_login() -> Response {
    let Some(cfg) = OidcConfig::from_env() else {
        return (StatusCode::NOT_FOUND, "SSO not configured").into_response();
    };
    let (Ok(state), Ok(nonce), Ok(verifier)) =
        (random_token(), random_token(), random_token())
    else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "oidc: RNG failure").into_response();
    };
    let challenge = pkce_challenge(&verifier);
    let eps = match fetch_endpoints(&cfg.issuer).await {
        Ok(e) if !e.authorization_endpoint.is_empty() => e,
        Ok(_) => {
            return (StatusCode::BAD_GATEWAY, "oidc: no authorization_endpoint").into_response()
        }
        Err(e) => {
            tracing::warn!("oidc discovery failed: {e}");
            return (StatusCode::BAD_GATEWAY, "oidc: IdP discovery failed").into_response();
        }
    };
    {
        let now = now_unix();
        if let Ok(mut flows) = oidc_flows().lock() {
            flows.retain(|_, (_, _, created)| now.saturating_sub(*created) < FLOW_TTL_SECS);
            flows.insert(state.clone(), (verifier, nonce.clone(), now));
        }
    }
    Redirect::to(&authorize_url(
        &eps.authorization_endpoint,
        &cfg,
        &state,
        &nonce,
        &challenge,
    ))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct CallbackQuery {
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

/// Complete an OIDC login: validate state, exchange the code, verify the
/// id-token, JIT the user, and issue a hub session.
pub(super) async fn oidc_callback(
    State(app): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let Some(cfg) = OidcConfig::from_env() else {
        return (StatusCode::NOT_FOUND, "SSO not configured").into_response();
    };
    // One-time use: remove the flow state regardless of outcome.
    let flow = match oidc_flows().lock() {
        Ok(mut flows) => flows.remove(&q.state),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "oidc: state store").into_response(),
    };
    let Some((verifier, nonce, created)) = flow else {
        return (StatusCode::BAD_REQUEST, "oidc: unknown or used state").into_response();
    };
    if now_unix().saturating_sub(created) >= FLOW_TTL_SECS {
        return (StatusCode::BAD_REQUEST, "oidc: login expired, try again").into_response();
    }
    let eps = match fetch_endpoints(&cfg.issuer).await {
        Ok(e) if !e.token_endpoint.is_empty() => e,
        Ok(_) => return (StatusCode::BAD_GATEWAY, "oidc: no token_endpoint").into_response(),
        Err(e) => {
            tracing::warn!("oidc discovery failed: {e}");
            return (StatusCode::BAD_GATEWAY, "oidc: IdP discovery failed").into_response();
        }
    };
    // Exchange the authorization code for tokens (PKCE verifier proves the
    // request came from the same client that started the flow).
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "oidc: http client").into_response();
    };
    let form = [
        ("grant_type", "authorization_code"),
        ("code", q.code.as_str()),
        ("redirect_uri", cfg.redirect_uri.as_str()),
        ("client_id", cfg.client_id.as_str()),
        ("client_secret", cfg.client_secret.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    let token_resp = match client
        .post(&eps.token_endpoint)
        .form(&form)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    {
        Ok(r) => r.json::<TokenResponse>().await,
        Err(e) => {
            tracing::warn!("oidc token exchange failed: {e}");
            return (StatusCode::BAD_GATEWAY, "oidc: token exchange failed").into_response();
        }
    };
    let Ok(tokens) = token_resp else {
        return (StatusCode::BAD_GATEWAY, "oidc: bad token response").into_response();
    };
    // Verify the id-token against the IdP JWKS, binding the stashed nonce.
    let jwks = match fetch_jwks(&cfg.issuer).await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("oidc jwks fetch failed: {e}");
            return (StatusCode::BAD_GATEWAY, "oidc: jwks fetch failed").into_response();
        }
    };
    let now_i64 = i64::try_from(now_unix()).unwrap_or(i64::MAX);
    let identity = match verify_id_token(&tokens.id_token, &cfg, &jwks, now_i64, Some(&nonce)) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("oidc id-token rejected: {e}");
            return (StatusCode::UNAUTHORIZED, "oidc: id-token rejected").into_response();
        }
    };
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_FOUND, "auth is not configured").into_response();
    };
    let username = identity
        .email
        .clone()
        .unwrap_or_else(|| identity.subject.clone());
    let role = role_from_groups(&identity.groups);
    // JIT provision/update: create_user is a no-op (false) for an existing
    // user; update_user then refreshes the role + email from the IdP each login.
    let pw = random_token().unwrap_or_else(|_| identity.subject.clone());
    let _ = auth.create_user(&username, &pw, role).await;
    let _ = auth
        .update_user(&username, "", identity.email.as_deref().unwrap_or(""), Some(role))
        .await;
    let Some(token) = auth.issue_session(&username).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "oidc: could not issue session").into_response();
    };
    audit_push(&app.audit, &username, "login (sso)".to_owned(), 200).await;
    // SameSite=Lax so the cookie survives the cross-site redirect back from the
    // IdP (Strict would drop it on the top-level GET navigation).
    let cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age=43200{}",
        cookie_secure(&headers)
    );
    ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_is_404_when_sso_is_unconfigured() {
        for k in [
            "COXAGENT_OIDC_ISSUER",
            "COXAGENT_OIDC_CLIENT_ID",
            "COXAGENT_OIDC_CLIENT_SECRET",
            "COXAGENT_OIDC_REDIRECT_URI",
        ] {
            std::env::remove_var(k);
        }
        let resp = oidc_login().await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn flows_prune_keeps_only_fresh_entries() {
        // Pure check of the TTL predicate used in the handler.
        let now = 10_000u64;
        let fresh = now.saturating_sub(now - 10) < FLOW_TTL_SECS; // 10s old
        let stale = now.saturating_sub(now - 700) < FLOW_TTL_SECS; // 700s old
        assert!(fresh);
        assert!(!stale);
    }
}
