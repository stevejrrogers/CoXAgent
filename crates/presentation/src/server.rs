//! HTTP server (inbound adapter) — serves the embedded dashboard, a JSON API
//! over the project state, an SSE stream for live updates, and control
//! endpoints that drive the hosted cycle runner (resume / pause / step / stop).

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use coxagent_application::metrics;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::use_cases::RunnerHandle;
use coxagent_application::Config;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::{Stream, StreamExt};

/// The embedded single-page dashboard.
const INDEX_HTML: &str = include_str!("web/index.html");

/// How often the SSE stream pushes a fresh snapshot.
const STREAM_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone)]
struct AppState {
    store: Arc<dyn StateStorePort>,
    runner: Arc<RunnerHandle>,
    config_path: PathBuf,
}

/// Serve the dashboard and API on `port`, driving the given runner.
///
/// # Errors
/// Returns an IO error if the port cannot be bound.
pub async fn serve(
    store: Arc<dyn StateStorePort>,
    runner: Arc<RunnerHandle>,
    config_path: PathBuf,
    port: u16,
) -> std::io::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/state", get(state))
        .route("/api/metrics", get(metrics_endpoint))
        .route("/api/runner", get(runner_status))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/control/:action", post(control))
        .route("/api/events", get(events))
        .with_state(AppState {
            store,
            runner,
            config_path,
        });

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

async fn runner_status(State(app): State<AppState>) -> impl IntoResponse {
    Json(app.runner.snapshot())
}

/// Current config (engine-per-role mapping, workflow, architecture rules).
async fn get_config(State(app): State<AppState>) -> impl IntoResponse {
    let cfg = std::fs::read_to_string(&app.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    Json(cfg)
}

/// Replace config. Takes effect on the next runner restart.
async fn put_config(State(app): State<AppState>, Json(cfg): Json<Config>) -> impl IntoResponse {
    match serde_json::to_string_pretty(&cfg) {
        Ok(text) => match std::fs::write(&app.config_path, text) {
            Ok(()) => {
                Json(serde_json::json!({ "ok": true, "note": "restart to apply" })).into_response()
            }
            Err(e) => internal_error(&e.to_string()),
        },
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Drive the runner. `action` is one of resume | pause | step | stop.
async fn control(State(app): State<AppState>, Path(action): Path<String>) -> impl IntoResponse {
    match action.as_str() {
        "resume" => app.runner.resume(),
        "pause" => app.runner.pause(),
        "step" => app.runner.step(),
        "stop" => app.runner.stop(),
        other => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("unknown action {other}") })),
            )
                .into_response()
        }
    }
    Json(app.runner.snapshot()).into_response()
}

/// SSE stream: pushes `{state, runner}` every second.
async fn events(State(app): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = IntervalStream::new(tokio::time::interval(STREAM_INTERVAL)).then(move |_| {
        let app = app.clone();
        async move {
            let state = app.store.load().await.ok();
            let payload = serde_json::json!({
                "state": state,
                "runner": app.runner.snapshot(),
            });
            Ok(Event::default().data(payload.to_string()))
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
