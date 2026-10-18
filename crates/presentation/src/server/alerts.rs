// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
//! Outbound alert delivery history (CXA-F235): what the runner spooled to the
//! webhook, what got acknowledged, what is retrying, what died — plus the
//! one-click replay that requeues a dead alert for immediate delivery.

use super::*;

/// One spooled alert as the dashboard sees it: the stored entry plus the
/// derived presentation flag (a pending entry with failed attempts reads as
/// "failed, retrying" rather than a first send in flight).
#[derive(serde::Serialize)]
struct AlertView {
    #[serde(flatten)]
    entry: coxagent_application::OutboxEntry,
    retrying: bool,
}

/// `GET /api/projects/:pid/alerts` — recent outbound alerts, newest first.
pub(super) async fn list_alerts_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let entries = match &p.outbox {
        Some(outbox) => outbox.recent(200).await,
        // No webhook sink configured for this project: no outbound history.
        None => Vec::new(),
    };
    let views: Vec<AlertView> = entries
        .into_iter()
        .map(|entry| AlertView {
            retrying: entry.is_retrying(),
            entry,
        })
        .collect();
    axum::Json(views).into_response()
}

/// `POST /api/projects/:pid/alerts/:id/replay` — requeue one dead alert for
/// immediate delivery (attempts reset, idempotency key unchanged).
pub(super) async fn replay_alert_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(id) = id.parse::<u64>() else {
        return (StatusCode::BAD_REQUEST, "alert id must be a number").into_response();
    };
    let Some(outbox) = &p.outbox else {
        return not_found();
    };
    if outbox.replay(id).await {
        axum::Json(serde_json::json!({ "replayed": id })).into_response()
    } else {
        not_found()
    }
}
