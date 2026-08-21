// Split from server/mod.rs — CORS allowlist + auth rate-limit wiring tests
// (COX-C016). Exercises the actual `cors_layer`/`rate_limit_mw`/`RateLimiter`
// used by `serve_full`'s `Router::new()` chain, mounted on a representative
// router (spinning up the full hub `Router` needs a fully-wired `AppState`,
// which is out of scope for a middleware-wiring test).
#![allow(clippy::wildcard_imports)]
use super::*;
use axum::body::Body;
use tower::ServiceExt;

const ALLOWED_ORIGIN: &str = "https://allowed.example.com";
const DISALLOWED_ORIGIN: &str = "https://not-allowed.example.com";

/// Same layer order as `serve_full`: CORS applied first, then the auth
/// rate-limit layer wraps outermost. `max`/`window` are parameterized so
/// tests can exhaust the limit in a couple of requests.
fn test_router(max: usize, window: Duration) -> Router {
    let app = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/auth/login", post(|| async { "logged in" }))
        .route("/api/auth/sessions", get(|| async { "sessions" }));

    let cors = cors_layer(ALLOWED_ORIGIN).expect("allowlist is non-empty");
    let app = app.layer(cors);

    let limiter = Arc::new(RateLimiter::new());
    app.layer(axum::middleware::from_fn(move |req, next| {
        rate_limit_mw(req, next, Arc::clone(&limiter), max, window, false)
    }))
}

fn get_request(uri: &str, origin: Option<&str>) -> Request<Body> {
    method_request("GET", uri, origin)
}

fn method_request(method: &str, uri: &str, origin: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn ac1_cors_layer_echoes_allowlisted_origin_never_a_wildcard() {
    let response = test_router(100, Duration::from_secs(60))
        .oneshot(get_request("/api/health", Some(ALLOWED_ORIGIN)))
        .await
        .unwrap();

    let acao = response
        .headers()
        .get("access-control-allow-origin")
        .map(|v| v.to_str().unwrap().to_owned());
    assert_eq!(
        acao.as_deref(),
        Some(ALLOWED_ORIGIN),
        "CORS must echo the exact allowlisted origin, never `*`"
    );
}

#[tokio::test]
async fn ac2_disallowed_origin_receives_no_cors_headers() {
    let response = test_router(100, Duration::from_secs(60))
        .oneshot(get_request("/api/health", Some(DISALLOWED_ORIGIN)))
        .await
        .unwrap();

    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "an origin outside the allowlist must not receive CORS headers"
    );
}

#[tokio::test]
async fn ac3_ac4_rate_limit_covers_every_api_auth_route_not_just_login() {
    // AC: "A rate-limiting layer is applied in front of /api/auth/* routes"
    // and "exceeding the limit against /api/auth/* returns 429". The
    // literal AC scope is the whole /api/auth/* surface — check a
    // non-login/non-2FA route (today only /api/auth/login and
    // /api/auth/2fa/* are limited; see rate_limit.rs's `path` check).
    let router = test_router(1, Duration::from_secs(60));

    let first = router
        .clone()
        .oneshot(get_request("/api/auth/sessions", None))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = router
        .oneshot(get_request("/api/auth/sessions", None))
        .await
        .unwrap();
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a second request to any /api/auth/* route beyond the limit must return 429"
    );
}

#[tokio::test]
async fn ac5_allowlisted_origin_under_limit_reaches_auth_handler_with_cors_headers() {
    let response = test_router(100, Duration::from_secs(60))
        .oneshot(method_request(
            "POST",
            "/api/auth/login",
            Some(ALLOWED_ORIGIN),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some(ALLOWED_ORIGIN)
    );
}
