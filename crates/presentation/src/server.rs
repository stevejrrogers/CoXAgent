//! HTTP server (inbound adapter) — serves the embedded dashboard and a JSON
//! API over the project state. Read-only in M5; control endpoints (pause,
//! trigger, chat) land with the teamwork milestone.

use axum::extract::State;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use coxagent_application::ports::outbound::StateStorePort;
use std::sync::Arc;

/// The embedded single-page dashboard.
const INDEX_HTML: &str = include_str!("web/index.html");

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
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
