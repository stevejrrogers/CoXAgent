// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The Wiki surface: pages, folders, AI edits, and the docs websocket.

use super::*;

/// List the project's documentation pages.
pub(super) async fn docs_list_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let docs = app.doc_list(&pid, &p).await;
    Json(docs).into_response()
}

/// Query parameters for the wiki sidebar search.
#[derive(serde::Deserialize)]
pub(super) struct WikiSearchParams {
    q: Option<String>,
}

/// In-wiki search (CXA-F364): titles + bodies, ranked with snippets — the
/// backend of the docs rail's filter box. Matching is pure
/// (`application::wiki_views::wiki_search`); a blank query returns an empty
/// list, which the rail reads as "unfiltered".
pub(super) async fn wiki_search_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Query(params): Query<WikiSearchParams>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let pages = app.doc_list(&pid, &p).await;
    Json(coxagent_application::wiki_views::wiki_search(
        &pages,
        params.q.as_deref().unwrap_or_default(),
    ))
    .into_response()
}

/// Backlinks for one page (CXA-F364): every sibling page body and ticket
/// description that references it, so the reader view can list "referenced
/// by" beneath the body. Unknown page → 404 like every doc route.
pub(super) async fn doc_backlinks_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(page) = app.doc_get(&pid, &p, &id).await else {
        return not_found();
    };
    let pages = app.doc_list(&pid, &p).await;
    let tickets = p.store.load().await.map(|s| s.tickets).unwrap_or_default();
    let mut links = coxagent_application::wiki_views::page_backlinks(&pages, &page.id, &page.title);
    links.extend(coxagent_application::wiki_views::ticket_backlinks(
        &tickets,
        &page.id,
        &page.title,
    ));
    Json(links).into_response()
}

/// Mint an id for a brand-new page (used only when the client sends none).
pub(super) fn mint_doc_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("doc-{nanos}")
}

/// Colour bucket from a folder path's top segment.
pub(super) fn doc_category(folder: &str) -> &'static str {
    match folder
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "technical" | "architecture" | "engineering" => "technical",
        "flows" | "design" => "flows",
        "testing" | "qa" | "test" | "tests" => "qa",
        "operations" | "ops" | "release notes" | "releases" => "ops",
        _ => "product",
    }
}

/// Create or update a documentation page.
pub(super) async fn doc_upsert_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<DocUpsertReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    if req.title.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "title required").into_response();
    }
    let author = resolve_username(&app, &headers).await;
    // A client-minted "new-<ts>" placeholder id means "create"; treat as empty.
    let id = if id.starts_with("new-") {
        ""
    } else {
        id.as_str()
    };
    match app
        .doc_upsert(
            &pid,
            &p,
            id,
            req.folder.trim(),
            req.title.trim(),
            &req.body,
            &author,
        )
        .await
    {
        Ok(page) => Json(page).into_response(),
        Err(e) => internal_error(&e),
    }
}

/// Ask the DOCS agent to revise one page per a human instruction.
pub(super) async fn doc_ai_edit_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    Json(req): Json<DocEditReq>,
) -> axum::response::Response {
    use coxagent_application::use_cases::GenerateDocsUseCase;
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(page) = app.doc_get(&pid, &p, &id).await else {
        return not_found();
    };
    if req.instruction.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "instruction required").into_response();
    }
    let uc = GenerateDocsUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
    );
    match uc
        .revise(
            &page.folder,
            &page.title,
            &page.body,
            req.instruction.trim(),
        )
        .await
    {
        Ok(body) => match app
            .doc_upsert(&pid, &p, &id, &page.folder, &page.title, &body, "DOCS")
            .await
        {
            Ok(saved) => Json(saved).into_response(),
            Err(e) => internal_error(&e),
        },
        Err(e) => internal_error(&format!("doc edit failed: {e}")),
    }
}

/// Delete a documentation page.
pub(super) async fn doc_delete_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    match app.doc_delete(&pid, &p, &id).await {
        Ok(removed) => Json(serde_json::json!({ "ok": removed })).into_response(),
        Err(e) => internal_error(&e),
    }
}

/// The explicit Wiki folder paths (empty folders included).
pub(super) async fn doc_folders_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let folders = p
        .store
        .load()
        .await
        .map(|s| s.doc_folders)
        .unwrap_or_default();
    Json(folders).into_response()
}

/// Create a (possibly nested) Wiki folder.
pub(super) async fn doc_folder_add_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<FolderReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(mut s) = p.store.load().await else {
        return internal_error("load failed");
    };
    s.add_doc_folder(&req.path);
    match p.store.save(&s).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Delete a Wiki folder and everything under it (subfolders + pages).
pub(super) async fn doc_folder_del_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<FolderReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let path = req.path.trim().trim_matches('/').to_owned();
    let prefix = format!("{path}/");
    // Delete pages under the folder from whichever store holds them.
    for page in app.doc_list(&pid, &p).await {
        if page.folder == path || page.folder.starts_with(&prefix) {
            let _ = app.doc_delete(&pid, &p, &page.id).await;
        }
    }
    let Ok(mut s) = p.store.load().await else {
        return internal_error("load failed");
    };
    s.remove_doc_folder(&path);
    match p.store.save(&s).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Move a page into another folder (works across the state/Mongo stores).
pub(super) async fn doc_move_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    Json(req): Json<FolderReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(page) = app.doc_get(&pid, &p, &id).await else {
        return (StatusCode::NOT_FOUND, "no such page").into_response();
    };
    match app
        .doc_upsert(&pid, &p, &id, &req.path, &page.title, &page.body, "USER")
        .await
    {
        Ok(_) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e),
    }
}

/// Generate/refresh the documentation with the DOCS agent.
pub(super) async fn docs_generate_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    use coxagent_application::use_cases::GenerateDocsUseCase;
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let md = tokio::fs::read_to_string(&p.context_path)
        .await
        .unwrap_or_default();
    let uc = GenerateDocsUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
    )
    .with_token_saver(project_token_saver(&p));
    let pages = match uc.execute(&md).await {
        Ok(pages) => pages,
        Err(e) => return internal_error(&format!("docs generation failed: {e}")),
    };
    let mut written = 0usize;
    for page in &pages {
        if app
            .doc_upsert(
                &pid,
                &p,
                &page.id,
                &page.folder,
                &page.title,
                &page.body,
                &page.updated_by,
            )
            .await
            .is_ok()
        {
            written += 1;
        }
    }
    Json(serde_json::json!({ "ok": true, "pages": written })).into_response()
}

/// Live collaborative-edit WebSocket for one documentation page. Same-origin
/// guarded + authenticated. Peers in the room see each other's edits and a live
/// presence roster; every save is persisted through the active doc store.
pub(super) async fn docs_ws_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
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
    if app.project(&pid).await.is_none() {
        return not_found();
    }
    let ws = ws.max_message_size(256 * 1024);
    ws.on_upgrade(move |socket| docs_socket(socket, app, pid, id, user))
}

/// Drive one live-edit socket: broadcast presence on join/leave, persist + fan
/// out each save to the room.
pub(super) async fn docs_socket(
    mut socket: WebSocket,
    app: AppState,
    pid: String,
    id: String,
    user: String,
) {
    let room = format!("{pid}/{id}");
    let tx = app.docs_room(&room).await;
    let mut rx = tx.subscribe();
    // Announce arrival and push the fresh roster to everyone (incl. this socket).
    let roster = app.docs_presence(&room, &user, true);
    let _ = tx.send(presence_json(&roster));
    loop {
        tokio::select! {
            bcast = rx.recv() => {
                match bcast {
                    Ok(json) => {
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
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                match v.get("op").and_then(|o| o.as_str()) {
                    Some("save") => {
                        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or_default();
                        let folder = v.get("folder").and_then(|x| x.as_str()).unwrap_or_default();
                        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or_default();
                        let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or_default();
                        if title.trim().is_empty() { continue; }
                        let Some(p) = app.project(&pid).await else { continue };
                        if app.doc_upsert(&pid, &p, &id, folder, title, body, &user).await.is_ok() {
                            let out = serde_json::json!({
                                "op": "doc", "id": id, "title": title, "folder": folder,
                                "body": body, "by": user, "origin": origin,
                            });
                            let _ = tx.send(out.to_string());
                        }
                    }
                    // A pure "typing" ping keeps presence lively without a save.
                    Some("ping") => {
                        let roster = {
                            let mut m = match app.docs_editors.lock() { Ok(m) => m, Err(p) => p.into_inner() };
                            m.entry(room.clone()).or_default().keys().cloned().collect::<Vec<_>>()
                        };
                        let _ = tx.send(presence_json(&roster));
                    }
                    _ => {}
                }
            }
        }
    }
    let roster = app.docs_presence(&room, &user, false);
    let _ = tx.send(presence_json(&roster));
}

/// On-demand DOCS Wiki-gap review: writes missing pages in full.
pub(super) async fn docs_review_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    let uc = coxagent_application::use_cases::RunDocsAuditUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
        cfg.workflow.language,
    );
    match uc.execute(current_sprint(&p).await).await {
        Ok(written) => Json(serde_json::json!({ "ok": true, "written": written })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}
