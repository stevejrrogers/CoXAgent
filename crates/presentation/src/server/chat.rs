// One logical module split across files for merge-conflict surface, not an API
// boundary — the children reach back into the parent's items wholesale, and
// enumerating ~200 shared types here would turn every rename into a two-file
// edit. Wildcard is the honest shape of that relationship.
#![allow(clippy::wildcard_imports)]
//! The system chat surface: channels, DMs, reactions, uploads, delivery.
//!
//! Split from server.rs (9,600 lines) so a chat endpoint change stops
//! conflicting with every other endpoint change.

use super::*;

/// Persist one chat message and fan it out to every live WebSocket. The
/// per-project `write_lock` serializes the load→append→save so concurrent
/// senders can't lose each other's messages. Returns `false` if persistence
/// fails. `body` must already be validated (non-empty, length-capped).
pub(super) async fn deliver_chat(
    app: &AppState,
    p: &ProjectHandle,
    user: &str,
    body: &str,
    channel: &str,
    attachments: Vec<coxagent_application::Attachment>,
) -> bool {
    let ch = app.chat_channel(&p.id).await;
    let _guard = ch.write_lock.lock().await;
    let Ok(mut state) = p.store.load().await else {
        return false;
    };
    // Enforce membership: only people who can view a channel may post to it.
    match state.channel(channel) {
        Some(c) if c.can_view(user) => {}
        _ => return false,
    }
    state.post_chat_in(user, body, channel, attachments);
    let msg = state.chat.last().cloned();
    if p.store.save(&state).await.is_err() {
        return false;
    }
    if let Some(m) = msg {
        // Ignore send errors: a broadcast with no live receivers is fine.
        let _ = ch.tx.send(serde_json::to_string(&m).unwrap_or_default());
    }
    true
}

/// List a channel's team-chat messages (oldest first). Filters to `?channel=`
/// (default `#general`); returns empty for a channel the caller can't view.
pub(super) async fn chat_list_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ChatListQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let channel = q
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    let user = resolve_username(&app, &headers).await;
    let Ok(state) = p.store.load().await else {
        return Json(Vec::<coxagent_application::ChatMsg>::new()).into_response();
    };
    match state.channel(&channel) {
        Some(c) if c.can_view(&user) => {}
        _ => return Json(Vec::<coxagent_application::ChatMsg>::new()).into_response(),
    }
    let chat: Vec<_> = state
        .chat
        .into_iter()
        .filter(|m| m.channel == channel)
        .collect();
    Json(chat).into_response()
}

/// Post a team-chat message as the signed-in user. Any authenticated principal
/// may post (see the `/chat` carve-out in [`auth_mw`]); the author is the
/// resolved username, or `"user"` when auth is disabled.
pub(super) async fn chat_post_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PostChatReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let body = req.body.trim();
    if body.is_empty() && req.attachments.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "empty message").into_response();
    }
    if body.chars().count() > 2000 {
        return (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "message too long",
        )
            .into_response();
    }
    let user = resolve_username(&app, &headers).await;
    let channel = req
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    if deliver_chat(&app, &p, &user, body, &channel, req.attachments).await {
        // A human asking the TEAM in a channel deserves an answer there —
        // until now only the Scrum box had a listener, so channel questions
        // fell into the void. Trigger on an explicit mention or a question
        // mark; plain chatter stays human-to-human (no engine burn).
        let lower = body.to_lowercase();
        let wants_team = lower.contains("@team") || lower.contains("@cox") || body.contains('?');
        let from_human = !user.eq_ignore_ascii_case("system");
        if wants_team && from_human {
            let msg = body.to_owned();
            let reply_channel = channel.clone();
            let p2 = p.clone();
            // The author's role, so a gate command typed in chat honours the
            // same role map as the Inbox. Open mode (no auth) = None = operator.
            let actor_role = match app.auth.clone() {
                Some(auth) => resolve_principal(&auth, &headers).await.map(|u| u.role),
                None => None,
            };
            let cfg = std::fs::read_to_string(&p2.config_path)
                .ok()
                .and_then(|t| serde_json::from_str::<Config>(&t).ok())
                .unwrap_or_default();
            tokio::spawn(async move {
                let uc = coxagent_application::use_cases::RunChatReplyUseCase::new(
                    Arc::clone(&p2.store),
                    Arc::clone(&p2.engine),
                    p2.work_dir.clone(),
                    cfg.workflow.token_saver,
                    cfg.workflow.language,
                )
                .with_files(p2.files.clone())
                .with_reply_channel(Some(reply_channel))
                .with_actor_role(actor_role);
                let _ = uc.execute(&msg).await;
            });
        }
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        // Either persistence failed or the user isn't a member of the channel.
        (StatusCode::FORBIDDEN, "cannot post to this channel").into_response()
    }
}

/// Persist one system-chat message and fan it out to every live WebSocket.
/// Returns `false` if the user can't view the channel or persistence fails.
pub(super) async fn deliver_syschat(
    app: &AppState,
    user: &str,
    body: &str,
    channel: &str,
    attachments: Vec<coxagent_application::Attachment>,
) -> bool {
    let ctx = app.chat_context().await;
    let msg = {
        let mut sc = app.syschat.inner.lock().await;
        if !sc.can_view(channel, user, &ctx) {
            return false;
        }
        sc.post(user, body, channel, attachments);
        sc.chat.last().cloned()
    };
    app.syschat.save().await;
    if let Some(m) = msg {
        let _ = app
            .syschat
            .tx
            .send(serde_json::to_string(&m).unwrap_or_default());
    }
    true
}

/// List the channels the signed-in user can see (general + their projects + private).
pub(super) async fn syschat_channels_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    let channels = { app.syschat.inner.lock().await.channels_for(&user, &ctx) };
    Json(channels).into_response()
}

/// Create a private channel. Restricted to Admin + lead tier (can_create_channel).
pub(super) async fn syschat_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateChannelReq>,
) -> axum::response::Response {
    let (user, can_create) = resolve_user_caps(&app, &headers).await;
    if !can_create {
        return (
            StatusCode::FORBIDDEN,
            "only leads and admins can create channels",
        )
            .into_response();
    }
    let ctx = app.chat_context().await;
    let result = {
        let mut sc = app.syschat.inner.lock().await;
        let kind = req.kind.as_deref().unwrap_or("private");
        sc.create_channel_with_kind(&req.name, &user, kind, &ctx)
    };
    match result {
        Ok(ch) => {
            app.syschat.save().await;
            (StatusCode::CREATED, Json(ch)).into_response()
        }
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

/// Invite a user to a private channel (or, with `delegate`, grant invite rights).
/// Channel settings on the SYSTEM chat store — privacy, who may invite, topic.
/// Channels are created through `/api/chat/channels`, so this is where their
/// settings must live too; the first version of this endpoint hung off the
/// per-project router and answered every request with "no such project".
pub(super) async fn syschat_settings_ep(
    State(app): State<AppState>,
    Path(cid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChannelSettingsReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let admin = user_can_manage(&app, &headers).await;
    // #general is synthesised when the channel list is served rather than
    // stored, so looking it up here finds nothing and used to answer 404 for
    // the one channel whose rule people are most likely to test.
    if cid == coxagent_application::state::GENERAL_CHANNEL {
        if req.kind.is_some() {
            return (
                StatusCode::BAD_REQUEST,
                "#general cannot be made private — a team needs one room nobody is shut out of",
            )
                .into_response();
        }
        return (
            StatusCode::BAD_REQUEST,
            "#general has no settings to change",
        )
            .into_response();
    }
    let mut sc = app.syschat.inner.lock().await;
    let Some(existing) = sc.channels.iter().find(|c| c.id == cid) else {
        return not_found();
    };
    if existing.owner != user && !admin {
        return (StatusCode::FORBIDDEN, "only the channel owner or an admin").into_response();
    }
    let updated = {
        let Some(ch) = sc.channels.iter_mut().find(|c| c.id == cid) else {
            return not_found();
        };
        if let Some(kind) = req.kind.as_deref() {
            if !ch.can_change_privacy() {
                return (
                    StatusCode::BAD_REQUEST,
                    "#general cannot be made private — a team needs one room nobody is shut out of",
                )
                    .into_response();
            }
            if !matches!(kind, "private" | "public") {
                return (StatusCode::BAD_REQUEST, "unknown channel kind").into_response();
            }
            kind.clone_into(&mut ch.kind);
        }
        if let Some(open) = req.open_invite {
            ch.open_invite = open;
        }
        if let Some(topic) = req.topic.as_deref() {
            ch.topic = topic.trim().chars().take(200).collect();
        }
        ch.clone()
    };
    drop(sc);
    app.syschat.save().await;
    Json(updated).into_response()
}

/// Remove a member from a system-chat channel: the owner, a delegated inviter,
/// or an admin. The owner cannot be removed from their own room.
pub(super) async fn syschat_kick_ep(
    State(app): State<AppState>,
    Path((cid, member)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let admin = user_can_manage(&app, &headers).await;
    let mut sc = app.syschat.inner.lock().await;
    let Some(ch) = sc.channels.iter().find(|c| c.id == cid) else {
        return not_found();
    };
    if !ch.can_kick(&user) && !admin {
        return (
            StatusCode::FORBIDDEN,
            "only the channel owner, a delegated inviter, or an admin",
        )
            .into_response();
    }
    if ch.owner == member {
        return (
            StatusCode::BAD_REQUEST,
            "the owner cannot be removed from their own channel",
        )
            .into_response();
    }
    let updated = {
        let Some(ch) = sc.channels.iter_mut().find(|c| c.id == cid) else {
            return not_found();
        };
        ch.members.retain(|m| m != &member);
        ch.inviters.retain(|m| m != &member);
        ch.clone()
    };
    drop(sc);
    app.syschat.save().await;
    Json(updated).into_response()
}

pub(super) async fn syschat_invite_ep(
    State(app): State<AppState>,
    Path(cid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChannelMemberReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let (result, channel) = {
        let mut sc = app.syschat.inner.lock().await;
        let r = if req.delegate {
            sc.delegate(&cid, &user, &req.user)
        } else {
            sc.invite(&cid, &user, &req.user)
        };
        let ch = sc.channels.iter().find(|c| c.id == cid).cloned();
        (r, ch)
    };
    match result {
        Ok(()) => {
            app.syschat.save().await;
            Json(channel).into_response()
        }
        Err(msg) => (StatusCode::FORBIDDEN, msg).into_response(),
    }
}

/// List a channel's messages (oldest first). Empty for channels the user can't view.
/// Split a hub sub-channel id (`cox-approvals`) into the project it belongs
/// to and the room inside that project's own chat (`approvals`).
///
/// The two chats are deliberately separate stores: a project's conversation
/// is part of its state, which is what the agents write to. Rather than
/// migrate that, the hub API is the door — it routes these ids through to
/// the project store so one sidebar shows both worlds.
fn project_sub_channel(app_projects: &[String], id: &str) -> Option<(String, String)> {
    for suffix in ["agents", "approvals"] {
        if let Some(prefix) = id.strip_suffix(&format!("-{suffix}")) {
            if app_projects.iter().any(|p| p == prefix) {
                return Some((prefix.to_owned(), suffix.to_owned()));
            }
        }
    }
    None
}

pub(super) async fn syschat_messages_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ChatListQuery>,
) -> axum::response::Response {
    let channel = q
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    // A project sub-channel reads from that project's own chat.
    let ids: Vec<String> = ctx.projects.iter().map(|p| p.id.clone()).collect();
    if let Some((pid, room)) = project_sub_channel(&ids, &channel) {
        let Some(p) = app.project(&pid).await else {
            return not_found();
        };
        let Ok(state) = p.store.load().await else {
            return internal_error("load failed");
        };
        return Json(paginate_tail(state.chat_in(&room), q.limit, q.before.as_deref()))
            .into_response();
    }
    let sc = app.syschat.inner.lock().await;
    if !sc.can_view(&channel, &user, &ctx) {
        return Json(Vec::<coxagent_application::ChatMsg>::new()).into_response();
    }
    Json(paginate_tail(
        sc.messages_in(&channel),
        q.limit,
        q.before.as_deref(),
    ))
    .into_response()
}

/// Newest-`limit` slice of a chronologically ordered message list, optionally
/// only messages strictly OLDER than the `before` id (the load-more cursor).
/// Order is preserved (oldest→newest within the returned window).
fn paginate_tail(
    msgs: Vec<coxagent_application::ChatMsg>,
    limit: Option<usize>,
    before: Option<&str>,
) -> Vec<coxagent_application::ChatMsg> {
    let limit = limit.unwrap_or(50).clamp(1, 500);
    let upper = match before {
        Some(id) => match msgs.iter().position(|m| m.id == id) {
            Some(i) => i,
            None => msgs.len(), // unknown cursor: serve the newest window
        },
        None => msgs.len(),
    };
    let lower = upper.saturating_sub(limit);
    msgs[lower..upper].to_vec()
}

/// Post a message to a system channel over REST (WS is the primary path).
pub(super) async fn syschat_send_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PostChatReq>,
) -> axum::response::Response {
    let body = req.body.trim();
    if body.is_empty() && req.attachments.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty message").into_response();
    }
    if body.chars().count() > CHAT_MAX_CHARS {
        return (StatusCode::PAYLOAD_TOO_LARGE, "message too long").into_response();
    }
    let user = resolve_username(&app, &headers).await;
    let channel = req
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    // A project sub-channel writes into that project's own chat, so a human
    // reply lands where the agents are already talking.
    {
        let ctx = app.chat_context().await;
        let ids: Vec<String> = ctx.projects.iter().map(|p| p.id.clone()).collect();
        if let Some((pid, room)) = project_sub_channel(&ids, &channel) {
            let Some(p) = app.project(&pid).await else {
                return not_found();
            };
            let Ok(mut state) = p.store.load().await else {
                return internal_error("load failed");
            };
            state.post_chat_in(&user, body, &room, req.attachments.clone());
            return match p.store.save(&state).await {
                Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
                Err(e) => internal_error(&e.to_string()),
            };
        }
    }
    if deliver_syschat(&app, &user, body, &channel, req.attachments).await {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::FORBIDDEN, "cannot post to this channel").into_response()
    }
}

/// List workspace members (minimal fields) for @mentions, member lists, and DMs.
/// Available to any signed-in user.
pub(super) async fn syschat_members_ep(State(app): State<AppState>) -> axum::response::Response {
    let members: Vec<serde_json::Value> = match &app.auth {
        Some(a) => a
            .list_users()
            .await
            .into_iter()
            .map(|u| serde_json::json!({ "username": u.username, "name": u.name }))
            .collect(),
        None => Vec::new(),
    };
    Json(members).into_response()
}

/// Open (or fetch) a direct-message channel with another user.
pub(super) async fn syschat_dm_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<DmReq>,
) -> axum::response::Response {
    let me = resolve_username(&app, &headers).await;
    let result = { app.syschat.inner.lock().await.open_dm(&me, req.user.trim()) };
    match result {
        Ok(ch) => {
            app.syschat.save().await;
            (StatusCode::CREATED, Json(ch)).into_response()
        }
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

/// Toggle the caller's emoji reaction on a message; broadcast the update.
pub(super) async fn syschat_react_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ReactReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let emoji = req.emoji.chars().take(8).collect::<String>();
    let updated = { app.syschat.inner.lock().await.react(&req.id, &user, &emoji) };
    let Some(msg) = updated else {
        return not_found();
    };
    app.syschat.save().await;
    // Broadcast a reaction event so every client updates the message in place.
    let evt = serde_json::json!({
        "type": "reaction", "channel": msg.channel, "id": msg.id, "reactions": msg.reactions,
    });
    let _ = app.syschat.tx.send(evt.to_string());
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Create an incoming webhook for a channel (any member). Returns the token +
/// the full post URL.
pub(super) async fn syschat_webhook_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<WebhookReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    let wh = {
        let mut sc = app.syschat.inner.lock().await;
        if !sc.can_view(&req.channel, &user, &ctx) {
            return (StatusCode::FORBIDDEN, "not a member of this channel").into_response();
        }
        sc.create_webhook(&req.channel, &req.label)
    };
    app.syschat.save().await;
    Json(serde_json::json!({
        "token": wh.token, "channel": wh.channel, "label": wh.label,
        "url": format!("/api/chat/hook/{}", wh.token),
    }))
    .into_response()
}

/// List a channel's webhooks (members only).
pub(super) async fn syschat_webhooks_list_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ChatListQuery>,
) -> axum::response::Response {
    let channel = q
        .channel
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    let sc = app.syschat.inner.lock().await;
    if !sc.can_view(&channel, &user, &ctx) {
        return Json(Vec::<coxagent_application::Webhook>::new()).into_response();
    }
    Json(sc.webhooks_for(&channel)).into_response()
}

/// Revoke a webhook by token.
pub(super) async fn syschat_webhook_delete_ep(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> axum::response::Response {
    let removed = { app.syschat.inner.lock().await.revoke_webhook(&token) };
    if removed {
        app.syschat.save().await;
    }
    Json(serde_json::json!({ "ok": removed })).into_response()
}

// ── Threads ──────────────────────────────────────────────────────────────
pub(super) async fn syschat_reply_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PostChatReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    if user.is_empty() {
        return (StatusCode::UNAUTHORIZED, "sign in").into_response();
    }
    let body = body.body.trim().to_owned();
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty message").into_response();
    }
    if body.len() > CHAT_MAX_CHARS {
        return (StatusCode::PAYLOAD_TOO_LARGE, "too long").into_response();
    }
    let mid_clone = mid.clone();
    let mut sc = app.syschat.inner.lock().await;
    let Some(channel) = sc
        .chat
        .iter()
        .find(|m| m.id == mid_clone)
        .map(|p| p.channel.clone())
    else {
        return (StatusCode::NOT_FOUND, "parent not found").into_response();
    };
    let msg = ChatMsg::reply(&user, &body, &channel, &mid_clone);
    sc.chat.push(msg.clone());
    if let Some(p) = sc.chat.iter_mut().find(|m| m.id == mid_clone) {
        p.reply_count = p.reply_count.saturating_add(1);
    }

    drop(sc);
    app.syschat.save().await;
    let frame = json!({ "op": "msg", "msg": msg });
    let _ = app.syschat.tx.send(frame.to_string());
    (StatusCode::CREATED, Json(msg)).into_response()
}

pub(super) async fn syschat_thread_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
) -> axum::response::Response {
    let sc = app.syschat.inner.lock().await;
    let parent = sc.chat.iter().find(|m| m.id == mid);
    let _channel = match parent {
        Some(p) => p.channel.clone(),
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let replies: Vec<ChatMsg> = sc
        .chat
        .iter()
        .filter(|m| m.thread_id.as_deref() == Some(&mid))
        .cloned()
        .collect();
    Json(serde_json::json!({ "parent": parent, "replies": replies })).into_response()
}

// ── Edit / Delete ────────────────────────────────────────────────────────
pub(super) async fn syschat_edit_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PostChatReq>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    if user.is_empty() {
        return (StatusCode::UNAUTHORIZED, "sign in").into_response();
    }
    let new_body = body.body.trim().to_owned();
    if new_body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty").into_response();
    }
    let mut sc = app.syschat.inner.lock().await;
    let Some(msg) = sc.chat.iter_mut().find(|m| m.id == mid) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if msg.user != user {
        return (StatusCode::FORBIDDEN, "not yours").into_response();
    }
    msg.body = new_body;
    msg.edited = Some(now_rfc3339());
    let edited = msg.clone();
    drop(sc);
    app.syschat.save().await;
    let frame = json!({ "op": "edit", "msg": edited });
    let _ = app.syschat.tx.send(frame.to_string());
    Json(edited).into_response()
}

pub(super) async fn syschat_delete_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    if user.is_empty() {
        return (StatusCode::UNAUTHORIZED, "sign in").into_response();
    }
    let mut sc = app.syschat.inner.lock().await;
    let Some(msg) = sc.chat.iter_mut().find(|m| m.id == mid) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if msg.user != user {
        return (StatusCode::FORBIDDEN, "not yours").into_response();
    }
    msg.deleted = true;
    msg.body = String::new();
    drop(sc);
    app.syschat.save().await;
    let frame = json!({ "op": "delete", "msg": { "id": mid } });
    let _ = app.syschat.tx.send(frame.to_string());
    Json(serde_json::json!({ "ok": true })).into_response()
}

// ── Search ───────────────────────────────────────────────────────────────
pub(super) async fn syschat_search_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let ctx = app.chat_context().await;
    let term = q.get("q").map(|s| s.to_lowercase()).unwrap_or_default();
    if term.is_empty() {
        return Json(Vec::<ChatMsg>::new()).into_response();
    }
    let sc = app.syschat.inner.lock().await;
    let results: Vec<ChatMsg> = sc
        .chat
        .iter()
        .filter(|m| {
            !m.deleted
                && sc.can_view(&m.channel, &user, &ctx)
                && (m.body.to_lowercase().contains(&term) || m.user.to_lowercase().contains(&term))
        })
        .rev()
        .take(50)
        .cloned()
        .collect();
    Json(results).into_response()
}

// ── Pin ──────────────────────────────────────────────────────────────────
pub(super) async fn syschat_pin_ep(
    State(app): State<AppState>,
    Path(mid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    if user.is_empty() {
        return (StatusCode::UNAUTHORIZED, "sign in").into_response();
    }
    let mid_clone = mid.clone();
    let mut sc = app.syschat.inner.lock().await;
    let Some(channel) = sc
        .chat
        .iter()
        .find(|m| m.id == mid_clone)
        .map(|m| m.channel.clone())
    else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let pins = sc.pins.entry(channel.clone()).or_default();
    if pins.contains(&mid_clone) {
        pins.retain(|p| p != &mid_clone);
    } else {
        pins.push(mid_clone.clone());
    }
    let pinned = pins.contains(&mid_clone);
    let pins_clone = pins.clone();
    drop(sc);
    app.syschat.save().await;
    let _ = app
        .syschat
        .tx
        .send(json!({ "op": "pin", "channel": channel, "pins": pins_clone }).to_string());
    Json(serde_json::json!({ "ok": true, "pinned": pinned })).into_response()
}

pub(super) async fn syschat_pins_ep(
    State(app): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let ch = q.get("channel").cloned().unwrap_or_default();
    let sc = app.syschat.inner.lock().await;
    let pins = sc.pins.get(&ch).cloned().unwrap_or_default();
    let msgs: Vec<ChatMsg> = pins
        .iter()
        .filter_map(|id| {
            sc.chat
                .iter()
                .find(|m| m.id == *id && !m.deleted && m.channel == ch)
        })
        .cloned()
        .collect();
    Json(msgs).into_response()
}

pub(super) async fn syschat_topic_ep(
    State(app): State<AppState>,
    Path(cid): Path<String>,
    Json(body): Json<TopicReq>,
) -> axum::response::Response {
    let topic = body.topic.trim().to_owned();
    let mut sc = app.syschat.inner.lock().await;
    sc.topic(&cid, topic);
    drop(sc);
    app.syschat.save().await;
    Json(serde_json::json!({"ok":true})).into_response()
}

pub(super) async fn syschat_topic_get_ep(
    State(app): State<AppState>,
    Path(cid): Path<String>,
) -> axum::response::Response {
    let sc = app.syschat.inner.lock().await;
    let topic = sc.get_topic(&cid);
    Json(serde_json::json!({"topic": topic})).into_response()
}

/// Public webhook endpoint: an external system posts a message to a channel
/// using only the secret token (no login). The token is the credential.
pub(super) async fn syschat_hook_ep(
    State(app): State<AppState>,
    Path(token): Path<String>,
    Json(req): Json<HookPostReq>,
) -> axum::response::Response {
    let Some((channel, label)) = ({ app.syschat.inner.lock().await.webhook(&token) }) else {
        return not_found();
    };
    let text = req.text.trim();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty text").into_response();
    }
    let author = if req.username.trim().is_empty() {
        label
    } else {
        req.username.trim().to_owned()
    };
    let msg = {
        let mut sc = app.syschat.inner.lock().await;
        let id = sc.post(
            &author,
            &text.chars().take(4000).collect::<String>(),
            &channel,
            Vec::new(),
        );
        sc.chat.iter().find(|m| m.id == id).cloned()
    };
    app.syschat.save().await;
    if let Some(m) = msg {
        let _ = app
            .syschat
            .tx
            .send(serde_json::to_string(&m).unwrap_or_default());
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Whether `user` may receive a system-chat broadcast (membership per message).
pub(super) async fn syschat_may_see(app: &AppState, user: &str, json: &str) -> bool {
    let channel = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("channel").and_then(|c| c.as_str()).map(str::to_owned))
        .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
    if channel == coxagent_application::GENERAL_CHANNEL {
        return true;
    }
    let ctx = app.chat_context().await;
    app.syschat
        .inner
        .lock()
        .await
        .can_view(&channel, user, &ctx)
}

/// Live system-chat WebSocket (same-origin guarded, authenticated).
pub(super) async fn syschat_ws_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Some(resp) = role_guard(true) {
        return resp;
    }
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    let user = match &app.auth {
        Some(auth) => match resolve_principal(auth, &headers).await {
            Some(u) => u.username,
            None => return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response(),
        },
        None => "user".to_owned(),
    };
    let ws = ws.max_message_size(64 * 1024);
    ws.on_upgrade(move |socket| syschat_socket(socket, app, user))
}

/// Drive one system-chat WebSocket: forward broadcasts the user may see, accept
/// validated, rate-limited messages from the client.
pub(super) async fn syschat_socket(mut socket: WebSocket, app: AppState, user: String) {
    let mut rx = app.syschat.tx.subscribe();
    let mut recv_times: std::collections::VecDeque<std::time::Instant> =
        std::collections::VecDeque::new();
    loop {
        tokio::select! {
            bcast = rx.recv() => {
                match bcast {
                    Ok(json) => {
                        // Call-signaling messages are addressed to one user; chat
                        // messages use channel membership.
                        let route_ok = match serde_json::from_str::<serde_json::Value>(&json) {
                            Ok(v) if v.get("type").and_then(|t| t.as_str()) == Some("signal") =>
                                v.get("to").and_then(|t| t.as_str()) == Some(user.as_str()),
                            _ => syschat_may_see(&app, &user, &json).await,
                        };
                        if !route_ok { continue; }
                        if socket.send(Message::Text(json)).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue,
                };
                let parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
                // WebRTC call signaling: stamp the sender and relay 1:1 to the
                // addressed user. Not persisted, not rate-limited.
                if parsed.as_ref().and_then(|v| v.get("type")).and_then(|t| t.as_str()) == Some("signal") {
                    if let Some(mut v) = parsed.clone() {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("from".to_owned(), serde_json::Value::String(user.clone()));
                            let _ = app.syschat.tx.send(v.to_string());
                        }
                    }
                    continue;
                }
                // Typing indicators: broadcast to other users, don't persist.
                if parsed.as_ref().and_then(|v| v.get("op")).and_then(|o| o.as_str()) == Some("typing") {
                    if let Some(mut v) = parsed.clone() {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("user".to_owned(), serde_json::Value::String(user.clone()));
                            let _ = app.syschat.tx.send(v.to_string());
                        }
                    }
                    continue;
                }
                let body = parsed.as_ref()
                    .and_then(|v| v.get("body").and_then(|b| b.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| text.clone());
                let channel = parsed.as_ref()
                    .and_then(|v| v.get("channel").and_then(|c| c.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
                let attachments: Vec<coxagent_application::Attachment> = parsed.as_ref()
                    .and_then(|v| v.get("attachments").cloned())
                    .and_then(|a| serde_json::from_value(a).ok())
                    .unwrap_or_default();
                let body = body.trim();
                if (body.is_empty() && attachments.is_empty()) || body.chars().count() > CHAT_MAX_CHARS {
                    continue;
                }
                let now = std::time::Instant::now();
                while recv_times.front().is_some_and(|t| now.duration_since(*t) > CHAT_RATE_WINDOW) {
                    recv_times.pop_front();
                }
                if recv_times.len() >= CHAT_RATE_MAX { continue; }
                recv_times.push_back(now);
                deliver_syschat(&app, &user, body, &channel, attachments).await;
            }
        }
    }
}

/// Upload a file for system chat; stored via the blob store (disk or S3/MinIO).
pub(super) async fn syschat_upload_ep(
    State(app): State<AppState>,
    mut multipart: axum::extract::Multipart,
) -> axum::response::Response {
    let Ok(Some(field)) = multipart.next_field().await else {
        return (StatusCode::BAD_REQUEST, "no file").into_response();
    };
    let orig = field.file_name().unwrap_or("file").to_owned();
    let mime = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_owned();
    let data = match field.bytes().await {
        Ok(b) if b.len() <= UPLOAD_MAX => b,
        Ok(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "file too large").into_response(),
        Err(_) => return (StatusCode::BAD_REQUEST, "read failed").into_response(),
    };
    let stored = format!("{}-{}", mint_media_token(), sanitize_name(&orig));
    if app
        .storage
        .put(&format!("chat/{stored}"), &data, &mime)
        .await
        .is_err()
    {
        return internal_error("write failed");
    }
    Json(serde_json::json!({
        "name": orig,
        "url": format!("/api/chat/media/{stored}"),
        "mime": mime,
        "size": data.len(),
    }))
    .into_response()
}

/// Serve a system-chat media file from the blob store (path-traversal guarded).
pub(super) async fn syschat_media_ep(
    State(app): State<AppState>,
    Path(file): Path<String>,
) -> axum::response::Response {
    if file.contains('/') || file.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let Ok(bytes) = app.storage.get(&format!("chat/{file}")).await else {
        return not_found();
    };
    let mut resp = (
        [
            (header::CONTENT_TYPE, mime_of(&file)),
            (header::CACHE_CONTROL, "private, max-age=31536000"),
        ],
        bytes,
    )
        .into_response();
    force_download_if_active_content(&file, &mut resp);
    resp
}

/// Live team-chat WebSocket. Requires an authenticated principal (enforced by
/// `auth_mw` on the upgrade GET, re-resolved here for the username) and — as
/// defense-in-depth against cross-site WebSocket hijacking on top of the
/// `SameSite=Strict` session cookie — a same-origin `Origin` header.
pub(super) async fn chat_ws_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Some(resp) = role_guard(true) {
        return resp;
    }
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Re-resolve the principal so the socket is attributed to a real user. When
    // auth is disabled (local mode) everyone is "user".
    let user = match &app.auth {
        Some(auth) => match resolve_principal(auth, &headers).await {
            Some(u) => u.username,
            None => {
                return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response();
            }
        },
        None => "user".to_owned(),
    };
    let ch = app.chat_channel(&pid).await;
    let mut ws = ws;
    ws = ws.max_message_size(64 * 1024);
    ws.on_upgrade(move |socket| chat_socket(socket, app, p, user, ch))
}

/// Drive one chat WebSocket in a single task: fan broadcast messages out to the
/// client while accepting validated, rate-limited messages from it. Using one
/// `select!` loop (rather than splitting the socket) avoids a `futures-util`
/// dependency — axum's `WebSocket` exposes async `recv`/`send` directly.
pub(super) async fn chat_socket(
    mut socket: WebSocket,
    app: AppState,
    p: ProjectHandle,
    user: String,
    ch: ChatChannel,
) {
    let mut rx = ch.tx.subscribe();
    let mut recv_times: std::collections::VecDeque<std::time::Instant> =
        std::collections::VecDeque::new();
    loop {
        tokio::select! {
            // Server → client: forward a broadcast message, but never leak a
            // private channel to a non-member — check membership per message.
            bcast = rx.recv() => {
                match bcast {
                    Ok(json) => {
                        if !user_may_see_broadcast(&p, &user, &json).await {
                            continue;
                        }
                        if socket.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    // Lagged (slow client) — skip missed messages, keep going.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            // Client → server: validate, rate-limit, persist + broadcast.
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue, // ignore binary/ping/pong
                };
                // Accept a raw string or {"body": "...", "attachments": [...]}.
                let parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
                let body = parsed
                    .as_ref()
                    .and_then(|v| v.get("body").and_then(|b| b.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| text.clone());
                let channel = parsed
                    .as_ref()
                    .and_then(|v| v.get("channel").and_then(|c| c.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| coxagent_application::GENERAL_CHANNEL.to_owned());
                let attachments: Vec<coxagent_application::Attachment> = parsed
                    .as_ref()
                    .and_then(|v| v.get("attachments").cloned())
                    .and_then(|a| serde_json::from_value(a).ok())
                    .unwrap_or_default();
                let body = body.trim();
                if (body.is_empty() && attachments.is_empty()) || body.chars().count() > CHAT_MAX_CHARS {
                    continue;
                }
                let now = std::time::Instant::now();
                while recv_times.front().is_some_and(|t| now.duration_since(*t) > CHAT_RATE_WINDOW) {
                    recv_times.pop_front();
                }
                if recv_times.len() >= CHAT_RATE_MAX {
                    continue; // silently drop; client is flooding
                }
                recv_times.push_back(now);
                deliver_chat(&app, &p, &user, body, &channel, attachments).await;
            }
        }
    }
}

/// A human posted in the team channel — the most relevant agent replies
/// intelligently and runs any action requested. Fired by the composer.
pub(super) async fn chat_reply_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChatReplyReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let actor_role = match app.auth.clone() {
        Some(auth) => resolve_principal(&auth, &headers).await.map(|u| u.role),
        None => None,
    };
    let msg = req.message.trim();
    if msg.is_empty() {
        return Json(serde_json::json!({ "ok": true })).into_response();
    }
    // One raw read feeds both the `Config` parse and the host-port probe, so
    // a malformed `deploy.host_port` can't drift from what `Config` saw and
    // fails the mandatory health gate (COX-B035) instead of silently
    // skipping it via `Config::default()`'s `host_port: None`.
    let raw_cfg = std::fs::read_to_string(&p.config_path).ok();
    let cfg = raw_cfg
        .as_deref()
        .and_then(|t| serde_json::from_str::<Config>(t).ok())
        .unwrap_or_default();
    // Independently parsed from the SAME raw text (COX-B035): distinguishes
    // "no host_port configured" from "host_port present but malformed",
    // which `cfg.deploy.host_port` alone cannot once a corrupt config has
    // already collapsed to `Config::default()` above.
    let host_port_probe = raw_cfg.as_deref().map_or(Ok(None), |t| {
        coxagent_application::ports::outbound::parse_deploy_host_port(t)
    });
    let mut uc = coxagent_application::use_cases::RunChatReplyUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
        cfg.workflow.token_saver,
        cfg.workflow.language,
    )
    .with_files(p.files.clone())
    .with_actor_role(actor_role);
    if let Some(d) = &p.deploy {
        uc = uc
            .with_deploy(Arc::clone(d))
            .with_host_port_probe(host_port_probe);
    }
    if let Some(f) = &p.forge {
        let target = if cfg.git.target_branch.trim().is_empty() {
            cfg.git.default_branch.clone()
        } else {
            cfg.git.target_branch.clone()
        };
        uc = uc.with_forge(Arc::clone(f), target, cfg.git.require_ci);
    }
    match uc.execute(msg).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// DELETE `/api/chat/channels/:cid` — archive a channel (owner or admin).
/// `#general` and `#agents` are permanent: a team needs one room nobody is
/// shut out of, and the agents room is where the machine reports.
pub(super) async fn syschat_delete_channel_ep(
    State(app): State<AppState>,
    Path(cid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let user = resolve_username(&app, &headers).await;
    let admin = user_can_manage(&app, &headers).await;
    if cid == coxagent_application::state::GENERAL_CHANNEL
        || cid == coxagent_application::state::AGENTS_CHANNEL
    {
        return (
            StatusCode::BAD_REQUEST,
            "#general and #agents are permanent rooms",
        )
            .into_response();
    }
    let mut sc = app.syschat.inner.lock().await;
    let Some(existing) = sc.channels.iter().find(|c| c.id == cid) else {
        return not_found();
    };
    if existing.owner != user && !admin {
        return (StatusCode::FORBIDDEN, "only the channel owner or an admin").into_response();
    }
    // Sub-channels go with their parent: an orphaned child is unreachable in
    // a tree UI.
    sc.channels.retain(|c| c.id != cid && c.parent != cid);
    drop(sc);
    app.syschat.save().await;
    Json(serde_json::json!({ "ok": true })).into_response()
}
