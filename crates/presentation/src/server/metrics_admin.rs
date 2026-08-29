// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
//! The metrics admin listener (CXA-C039): a separate, unauthenticated,
//! loopback-only axum server exposing operational metrics to a scraper.
//!
//! # Why a second listener
//! The scraper surface must exist even when the hub's main port is
//! authenticated and role-restricted, and it must never appear in the OpenAPI
//! service catalog: this router is built and served HERE (never chained onto
//! `serve_full`'s `Router::new()` in `mod.rs`), so the `openapi_routes_gate`
//! — which scans `mod.rs` for `.route(` literals — cannot see it. It binds
//! `127.0.0.1` only: deliberately not reachable from outside the host; a
//! Prometheus instance scrapes it locally or via a trusted sidecar.
//!
//! # Contract
//! * `GET /metrics` — `200`, `text/plain; version=0.0.4`, body from
//!   [`coxagent_application::encode_prometheus`].
//! * `GET /healthz` — `200`, `application/json`, `{"status":"ok"}`.
//! * any other path — `404`, empty body.
//!
//! # Failure posture
//! This listener is auxiliary: if its port is taken (e.g. two hubs on one
//! host), the hub starts anyway and the failure is logged loudly — losing
//! metrics must not take the dashboard down. Pick a distinct
//! `COXAGENT_METRICS_PORT` per instance when running N on one host.

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use coxagent_application::{encode_prometheus, MetricsRegistry, PROMETHEUS_CONTENT_TYPE};
use serde_json::json;
use std::sync::Arc;

/// Env var selecting the admin-listener port (documented in README).
pub(super) const METRICS_PORT_ENV: &str = "COXAGENT_METRICS_PORT";
/// Default admin-listener port when [`METRICS_PORT_ENV`] is unset.
pub(super) const DEFAULT_METRICS_PORT: u16 = 9010;

/// Build the admin-listener router for `registry`. Pure wiring — no IO — so
/// tests can drive it end to end with `tower::ServiceExt::oneshot`.
pub(super) fn metrics_admin_router(registry: Arc<MetricsRegistry>) -> Router {
    Router::new()
        .route("/metrics", get(metrics_ep))
        .route("/healthz", get(healthz_ep))
        .fallback(fallback_ep)
        .with_state(registry)
}

/// Resolve the admin port from the environment, defaulting when unset or
/// unparseable (a bad value must not block the hub from starting).
fn metrics_port() -> u16 {
    match std::env::var(METRICS_PORT_ENV) {
        Ok(raw) => raw.trim().parse().unwrap_or_else(|_| {
            tracing::warn!(
                "{METRICS_PORT_ENV}={raw:?} is not a port — using {DEFAULT_METRICS_PORT}"
            );
            DEFAULT_METRICS_PORT
        }),
        Err(_) => DEFAULT_METRICS_PORT,
    }
}

/// Bind the loopback admin listener and serve it for the life of the process.
///
/// Fire-and-forget by design: the hub must not refuse to start because the
/// metrics port is busy. Every failure path logs at error level.
pub(super) fn spawn_metrics_admin(registry: Arc<MetricsRegistry>) {
    let port = metrics_port();
    let router = metrics_admin_router(registry);
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                tracing::info!("metrics admin listener on http://127.0.0.1:{port}");
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::error!("metrics admin listener stopped: {e}");
                }
            }
            Err(e) => tracing::error!(
                "metrics admin listener could not bind 127.0.0.1:{port} ({e}) — \
                 set {} to a free port; the hub continues without it",
                METRICS_PORT_ENV
            ),
        }
    });
}

async fn metrics_ep(State(registry): State<Arc<MetricsRegistry>>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
        encode_prometheus(&registry),
    )
}

async fn healthz_ep() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn fallback_ep() -> StatusCode {
    StatusCode::NOT_FOUND
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn serve(path: &str) -> axum::response::Response {
        let registry = Arc::new(MetricsRegistry::new());
        metrics_admin_router(registry)
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn metrics_endpoint_serves_prometheus_exposition() {
        let response = serve("/metrics").await;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type header")
            .to_str()
            .expect("ascii header");
        assert_eq!(content_type, PROMETHEUS_CONTENT_TYPE);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("cxa_process_uptime_seconds"),
            "uptime gauge present: {text}"
        );
    }

    #[tokio::test]
    async fn healthz_endpoint_serves_ok_json() {
        let response = serve("/healthz").await;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type header")
            .to_str()
            .expect("ascii header");
        assert!(
            content_type.starts_with("application/json"),
            "{content_type}"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"status": "ok"})
        );
    }

    #[tokio::test]
    async fn other_paths_return_404_with_empty_body() {
        let response = serve("/api/secret").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty(), "404 body must be empty");
    }

    #[test]
    fn metrics_port_defaults_when_unset_or_garbage() {
        // Unset → default.
        std::env::remove_var(METRICS_PORT_ENV);
        assert_eq!(metrics_port(), DEFAULT_METRICS_PORT);
        // Garbage → default, not a panic (validated external input).
        std::env::set_var(METRICS_PORT_ENV, "not-a-port");
        assert_eq!(metrics_port(), DEFAULT_METRICS_PORT);
        std::env::remove_var(METRICS_PORT_ENV);
    }
}
