// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The discussion thread: comments, reactions, attachment analysis.

use super::*;

/// List discussion comments, optionally filtered to one ticket via `?ticket=ID`.
pub(super) async fn list_comments(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<CommentQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let mut comments = p.store.load().await.map(|s| s.comments).unwrap_or_default();
    if let Some(tid) = q.ticket {
        comments.retain(|c| c.ticket.as_deref() == Some(tid.as_str()));
    }
    Json(comments).into_response()
}

/// Post a comment (as the user) to a ticket thread or the team channel.
pub(super) async fn post_comment(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PostCommentReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let body = req.body.trim();
    if body.is_empty() && req.attachments.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "empty comment").into_response();
    }
    // Attribute the comment to the signed-in account (so Scrum/discussion shows
    // real names, not a generic "USER"); falls back to "USER" in open mode.
    let author = resolve_username(&app, &headers).await;
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    state.post_comment_att(&author, body, req.ticket.clone(), req.attachments.clone());
    if let Err(e) = p.store.save(&state).await {
        return internal_error(&e.to_string());
    }
    // If the user attached something an agent can read, let the SA agent read it
    // and respond — answering if a question was asked, otherwise reading it
    // proactively and asking back. Runs in the background so the post is instant.
    maybe_analyze_attachments(&app, &p, &author, body, req.ticket, &req.attachments);
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Toggle the caller's emoji reaction on a ticket/discussion comment.
pub(super) async fn comment_react_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CommentReactReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let user = resolve_username(&app, &headers).await;
    let emoji = req.emoji.chars().take(8).collect::<String>();
    if emoji.is_empty() {
        return (StatusCode::BAD_REQUEST, "emoji required").into_response();
    }
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(updated) = state.react_comment(&id, &user, &emoji) else {
        return not_found();
    };
    if let Err(e) = p.store.save(&state).await {
        return internal_error(&e.to_string());
    }
    Json(updated).into_response()
}

/// Spawn a background SA turn that reads any readable attachments on a freshly
/// posted comment and replies. No-op when there are no readable attachments.
pub(super) fn maybe_analyze_attachments(
    app: &AppState,
    p: &ProjectHandle,
    author: &str,
    body: &str,
    ticket: Option<String>,
    attachments: &[coxagent_application::Attachment],
) {
    use coxagent_application::use_cases::{AnalyzeAttachmentUseCase, ReadableAttachment};
    // Storage keys for each attachment (the CLI reads local files, so we fetch
    // the bytes from the blob store into a temp dir — works for disk and S3).
    let items: Vec<(String, String, String)> = attachments
        .iter()
        .filter_map(|a| {
            let file = a.url.rsplit('/').next()?;
            Some((
                a.name.clone(),
                a.mime.clone(),
                format!("proj/{}/{file}", p.id),
            ))
        })
        .collect();
    if items.is_empty() {
        return;
    }
    let storage = Arc::clone(&app.storage);
    let store = Arc::clone(&p.store);
    let engine = Arc::clone(&p.engine);
    let work_dir = p.work_dir.clone();
    let (author, body) = (author.to_owned(), body.to_owned());
    tokio::spawn(async move {
        let tmp = std::env::temp_dir().join(format!("cox-att-{}", mint_media_token()));
        let _ = std::fs::create_dir_all(&tmp);
        let mut readable: Vec<ReadableAttachment> = Vec::new();
        for (name, mime, key) in items {
            let Ok(bytes) = storage.get(&key).await else {
                continue;
            };
            let fname = key.rsplit('/').next().unwrap_or("file");
            let path = tmp.join(fname);
            if std::fs::write(&path, &bytes).is_ok() {
                readable.push(ReadableAttachment { name, mime, path });
            }
        }
        if !readable.is_empty() {
            let uc = AnalyzeAttachmentUseCase::new(store, engine, work_dir);
            if let Err(e) = uc.execute(&author, &body, ticket, &readable).await {
                tracing::warn!("attachment analysis failed: {e}");
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    });
}
