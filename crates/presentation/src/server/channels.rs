// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Per-project channel endpoints (the system-chat ones live in chat.rs).

use super::*;

/// List the channels the signed-in user can see (`#general` first).
pub(super) async fn channels_list_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let user = resolve_username(&app, &headers).await;
    let channels = p
        .store
        .load()
        .await
        .map(|s| s.channels_for(&user))
        .unwrap_or_default();
    Json(channels).into_response()
}

/// Create a private channel owned by the signed-in user.
pub(super) async fn channel_create_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateChannelReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let user = resolve_username(&app, &headers).await;
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let created = match req.parent.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(parent) => state.create_sub_channel(
            parent,
            &req.name,
            &user,
            req.kind.as_deref().unwrap_or("private"),
        ),
        None => state.create_channel_with_kind(
            &req.name,
            &user,
            req.kind.as_deref().unwrap_or("private"),
        ),
    };
    match created {
        Ok(ch) => match p.store.save(&state).await {
            Ok(()) => (StatusCode::CREATED, Json(ch)).into_response(),
            Err(e) => internal_error(&e.to_string()),
        },
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

/// Change a channel's settings (privacy, who may invite, topic). Owner or an
/// admin: a room's owner runs their room, and an admin outranks that.
pub(super) async fn channel_settings_ep(
    State(app): State<AppState>,
    Path((pid, cid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChannelSettingsReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let user = resolve_username(&app, &headers).await;
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(ch) = state.channels.iter().find(|c| c.id == cid) else {
        return not_found();
    };
    if ch.owner != user && !user_can_manage(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "only the channel owner or an admin").into_response();
    }
    match state.update_channel_settings(
        &cid,
        req.kind.as_deref(),
        req.open_invite,
        req.topic.as_deref(),
    ) {
        Ok(ch) => match p.store.save(&state).await {
            Ok(()) => Json(ch).into_response(),
            Err(e) => internal_error(&e.to_string()),
        },
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

/// Remove a member from a channel. The owner, a delegated inviter, or an admin.
pub(super) async fn channel_kick_ep(
    State(app): State<AppState>,
    Path((pid, cid, member)): Path<(String, String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let user = resolve_username(&app, &headers).await;
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(ch) = state.channels.iter().find(|c| c.id == cid) else {
        return not_found();
    };
    if !ch.can_kick(&user) && !user_can_manage(&app, &headers).await {
        return (
            StatusCode::FORBIDDEN,
            "only the channel owner, a delegated inviter, or an admin",
        )
            .into_response();
    }
    match state.remove_channel_member(&cid, &member) {
        Ok(ch) => match p.store.save(&state).await {
            Ok(()) => Json(ch).into_response(),
            Err(e) => internal_error(&e.to_string()),
        },
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}
