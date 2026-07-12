//! HTTP server (inbound adapter) — serves the embedded dashboard and a JSON
//! API over the project state, plus a Server-Sent Events stream for live
//! updates. Read-only in M5; control endpoints (pause, trigger, chat) land with
//! the teamwork milestone.

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use coxagent_application::metrics;
use coxagent_application::ports::outbound::StateStorePort;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::{Stream, StreamExt};

/// The embedded single-page dashboard.
const INDEX_HTML: &str = include_str!("web/index.html");

/// How often the SSE stream pushes a fresh snapshot.
const STREAM_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct AppState {
    store: Arc<dyn StateStorePort>,
}

/// Serve the dashboard and API on `port` until the process ends.
///
/// # Errors
/// Returns an IO error if the port cannot be bound.
pub async fn serve(store: Arc<dyn StateStorePort>, port: u16) -> std::io::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/state", get(state))
        .route("/api/metrics", get(metrics_endpoint))
        .route("/api/events", get(events))
        .with_state(AppState { store });

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("dashboard on http://{addr}");
    axum::serve(listener, app).await
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn state(State(app): State<AppState>) -> impl IntoResponse {
    match app.store.load().await {
        Ok(state) => Json(serde_json::to_value(state).unwrap_or_default()).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn metrics_endpoint(State(app): State<AppState>) -> impl IntoResponse {
    match app.store.load().await {
        Ok(state) => Json(metrics::compute(&state)).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// SSE stream that pushes the full state as a JSON `Event` every couple seconds.
async fn events(State(app): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let store = app.store;
    let stream = IntervalStream::new(tokio::time::interval(STREAM_INTERVAL)).then(move |_| {
        let store = store.clone();
        async move {
            let data = match store.load().await {
                Ok(s) => serde_json::to_string(&s).unwrap_or_else(|_| "{}".to_owned()),
                Err(_) => "{}".to_owned(),
            };
            Ok(Event::default().data(data))
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn internal_error(msg: &str) -> axum::response::Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}
