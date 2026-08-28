//! HTTP telemetry middleware: record one counter + one latency sample per
//! request into the application-layer [`MetricsRegistry`] (CXA-C039).
//!
//! Mirrors [`crate::middleware::rate_limit`]'s split: the pure decision (what
//! to record, under which labels) is a plain function over the request, and
//! the axum middleware is a thin inbound adapter around it.
//!
//! # Cardinality
//! The `route` label is the matched route PATTERN (`/api/projects/:pid`), not
//! the concrete path. Unmatched paths (404 fallthrough, scanner probes) go
//! through [`coxagent_application::label_bucket`] so dynamic ids collapse onto
//! a bounded family key instead of minting one series per URL.
//!
//! The middleware sits OUTERMOST on the hub router, so the status it records
//! is the response the client actually sees — including 429s from the
//! rate-limit layer and 5xx from inner fallthroughs — exactly once per
//! request.

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use coxagent_application::{
    label_bucket, MetricsRegistry, HTTP_REQUEST_DURATION, HTTP_REQUESTS,
};
use std::sync::Arc;
use std::time::Instant;

/// Axum middleware applying `registry` to every request.
///
/// Labels: `route` (matched pattern or bucketed path), `method`,
/// `status` (class: `2xx`…`5xx`, else `other`). Exactly one counter increment
/// and one duration observation per request.
pub async fn telemetry_mw(
    req: Request,
    next: Next,
    registry: Arc<MetricsRegistry>,
) -> Response {
    let started = Instant::now();
    let route = route_label(&req);
    let method = req.method().as_str().to_owned();

    let response = next.run(req).await;

    let status = status_class(response.status());
    let labels: &[(&str, &str)] = &[("route", &route), ("method", &method), ("status", &status)];
    registry.inc_counter(HTTP_REQUESTS, labels);
    registry.observe_duration(
        HTTP_REQUEST_DURATION,
        labels,
        started.elapsed().as_secs_f64(),
    );
    response
}

/// The route label for a request: the matched route pattern when the router
/// matched one, else the path bucketed into a bounded family.
fn route_label(req: &Request) -> String {
    req.extensions()
        .get::<MatchedPath>()
        .map_or_else(
            || label_bucket(req.uri().path()),
            |matched| matched.as_str().to_owned(),
        )
}

/// Coarse status class for the `status` label: `2xx`…`5xx`; anything outside
/// the 100–599 range (e.g. a raw 1xx upgrade path gone odd) buckets as
/// `other` so the label space stays fixed.
fn status_class(status: axum::http::StatusCode) -> String {
    match status.as_u16() {
        100..=599 => format!("{}xx", status.as_u16() / 100),
        _ => "other".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::middleware;
    use axum::routing::get;
    use axum::Router;
    use coxagent_application::encode_prometheus;
    use tower::ServiceExt;

    /// A one-route router whose handler answers `status` — the "fake Next"
    /// returning a chosen status — layered with the real telemetry middleware,
    /// exactly as `serve_full` layers it.
    fn app(status: StatusCode, registry: Arc<MetricsRegistry>) -> Router {
        Router::new()
            .route("/api/projects/:pid/metrics", get(move || async move { status }))
            .layer(middleware::from_fn(move |req, next| {
                telemetry_mw(req, next, Arc::clone(&registry))
            }))
    }

    #[tokio::test]
    async fn records_status_class_method_and_route_pattern_labels() {
        let registry = Arc::new(MetricsRegistry::new());
        app(StatusCode::OK, Arc::clone(&registry))
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/projects/demo/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let text = encode_prometheus(&registry);
        assert!(
            text.contains(
                "cxa_http_request_total{method=\"GET\",route=\"/api/projects/:pid/metrics\",status=\"2xx\"} 1"
            ),
            "matched route must record the PATTERN family: {text}"
        );
        let duration_count = text
            .lines()
            .find(|l| l.starts_with("cxa_http_request_duration_seconds_count"))
            .expect("duration histogram present");
        assert!(duration_count.ends_with(" 1"), "exactly one sample: {text}");
    }

    #[tokio::test]
    async fn records_each_request_exactly_once_not_twice() {
        let registry = Arc::new(MetricsRegistry::new());
        let a = app(StatusCode::OK, Arc::clone(&registry));
        a.oneshot(
            HttpRequest::builder()
                .uri("/api/projects/demo/metrics")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        app(StatusCode::INTERNAL_SERVER_ERROR, Arc::clone(&registry))
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/projects/demo/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let text = encode_prometheus(&registry);
        let totals: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("cxa_http_request_total{"))
            .collect();
        assert_eq!(
            totals.len(),
            2,
            "one series per distinct label set, one increment each: {text}"
        );
        assert!(text.contains("status=\"2xx\"} 1"), "{text}");
        assert!(text.contains("status=\"5xx\"} 1"), "{text}");
        // Two samples total — one per series, since the status labels differ.
        let count_sum: u64 = text
            .lines()
            .filter(|l| l.starts_with("cxa_http_request_duration_seconds_count"))
            .map(|l| l.rsplit_once(' ').expect("count has value").1.parse::<u64>().expect("count parses"))
            .sum();
        assert_eq!(count_sum, 2, "exactly one sample per request: {text}");
    }

    #[tokio::test]
    async fn unmatched_paths_bucket_into_bounded_family_labels() {
        let registry = Arc::new(MetricsRegistry::new());
        let app = app(StatusCode::NOT_FOUND, Arc::clone(&registry));
        for probe in ["/api/projects/one/nope", "/api/projects/two/nope", "/favicon.ico"] {
            app.clone()
                .oneshot(
                    HttpRequest::builder()
                        .uri(probe)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
        }
        let text = encode_prometheus(&registry);
        assert!(
            text.contains("route=\"/api/projects/:pid/*\""),
            "unmatched project paths collapse onto the :pid family: {text}"
        );
        assert!(
            text.contains("status=\"4xx\""),
            "a 404 answer is recorded as 4xx: {text}"
        );
    }
}
