//! SSO/OIDC browser-flow routes (slice 3).
//!
//! `GET /api/auth/oidc/login` starts an authorization-code + PKCE flow and
//! redirects to the IdP; `GET /api/auth/oidc/callback` (slice 3b) completes it.
//! Both are public (the user has no hub session yet) and GATED: with no
//! `COXAGENT_OIDC_*` configured they 404, so the live password-only deployment
//! is unchanged until an operator turns SSO on.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};

use coxagent_application::oidc::OidcConfig;
use coxagent_infrastructure::oidc_auth::{authorize_url, fetch_endpoints, pkce_challenge, random_token};

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
