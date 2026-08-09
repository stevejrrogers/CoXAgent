//! CORS middleware — wraps `tower_http::cors::CorsLayer` behind an env-var
//! driven origin allowlist.
//!
//! # Env var
//! `COXAGENT_CORS_ORIGINS` — comma-separated list of fully-qualified origins
//! (e.g. `https://app.example.com,https://admin.example.com`).
//!
//! * Unset or empty → returns `None`; no CORS layer is applied, identical to
//!   today's behaviour (same-origin + SameSite=Strict cookies guard the app).
//! * Malformed individual entries are dropped with a warning; they never panic
//!   the hub at boot.
//! * Allowed origins are echoed exactly — never `*`; credentials are always
//!   enabled because auth uses both the session cookie and the Authorization
//!   header.

use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

/// Parse a comma-separated origin allowlist string and return a configured
/// [`CorsLayer`], or `None` when the list is empty.
///
/// Each origin must be a valid `http::HeaderValue`-representable ASCII string
/// (e.g. `"https://app.example.com"`).  Invalid entries are logged and skipped.
#[must_use]
pub fn cors_layer(origins_env: &str) -> Option<CorsLayer> {
    use axum::http::HeaderValue;

    let origins: Vec<HeaderValue> = origins_env
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|origin| match origin.parse::<HeaderValue>() {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!(
                    origin,
                    "COXAGENT_CORS_ORIGINS: skipping malformed entry (not a valid header value)"
                );
                None
            }
        })
        .collect();

    if origins.is_empty() {
        return None;
    }

    Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_credentials(true)
            .allow_methods(AllowMethods::list([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::PUT,
                axum::http::Method::PATCH,
                axum::http::Method::DELETE,
                axum::http::Method::OPTIONS,
            ]))
            .allow_headers(AllowHeaders::list([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
                axum::http::header::COOKIE,
            ])),
    )
}

#[cfg(test)]
mod tests {
    use super::cors_layer;

    #[test]
    fn empty_string_yields_no_layer() {
        assert!(cors_layer("").is_none());
    }

    #[test]
    fn whitespace_only_yields_no_layer() {
        assert!(cors_layer("   ,  , ").is_none());
    }

    #[test]
    fn single_valid_origin_produces_a_layer() {
        let layer = cors_layer("https://app.example.com");
        assert!(layer.is_some());
    }

    #[test]
    fn multiple_valid_origins_produce_a_layer() {
        let layer = cors_layer("https://app.example.com, https://admin.example.com");
        assert!(layer.is_some());
    }

    #[test]
    fn malformed_entry_is_dropped_but_valid_entries_survive() {
        // A null byte embedded in a header value is illegal; the valid entry
        // should still produce a layer.
        let layer = cors_layer("https://good.example.com,not a valid origin\x00nul");
        // The null-byte entry is illegal; "good" should survive.
        assert!(layer.is_some(), "valid origin was discarded alongside the bad one");
    }

    #[test]
    fn all_malformed_yields_no_layer() {
        let layer = cors_layer("not\x00valid,also\x00bad");
        assert!(layer.is_none(), "should return None when every entry is malformed");
    }
}
