// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The Wiki surface: pages, folders, AI edits, and the docs websocket.

use super::*;
use coxagent_infrastructure::deploy::{
    orphaned_compose_image, reclaimable_compose_project, reclaimable_volume,
};

/// Docker janitor: agents deploy a lot — the host must not silt up. Hourly,
/// in four sweeps:
///
/// 1. any reclaimable compose project whose containers are ALL stopped gets a
///    full `down -v --remove-orphans` (dead previews, stale deploys), and any
///    PR preview still running past [`PREVIEW_TTL`] is reclaimed — `-v` so a
///    project this pass destroys cannot leave its volumes dormant behind
///    (CXA-B143);
/// 2. dormant volumes are swept: volumes labelled for a reclaimable project
///    that has not a single container left are removed — teardowns from
///    before the `-v` above (and outright-killed runs) left exactly that
///    residue on the host (CXA-B143);
/// 3. orphaned tagged images are swept: compose's built `<project>-<service>`
///    tags outlive every teardown because `image prune -f` only reaps
///    DANGLING layers, so each worktree stack used to leave ~375 MB behind
///    (CXA-B143);
/// 4. dangling images are pruned (unchanged).
///
/// Which resources are reclaimable is decided by the single shared policy in
/// `coxagent_infrastructure::deploy::reclaimable`: it excludes our own live
/// hub and backing services, so a janitor tick can never take production down.
/// The sweep errs on the side of keeping: label-less volumes, referenced
/// images, and anything whose project might still exist are all left alone.
pub(super) async fn docker_janitor() {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        // Phase 1 — reclaim dead / expired compose projects.
        for (name, status) in compose_projects_with_status().await {
            if !reclaimable_compose_project(&name) {
                continue;
            }
            let mut reason = "dead";
            if status.contains("running") {
                if !name.starts_with(PREVIEW_PROJECT_PREFIX)
                    || !preview_is_stale(&name, PREVIEW_TTL).await
                {
                    continue;
                }
                reason = "expired preview";
            }
            run_docker(&["compose", "-p", &name, "down", "-v", "--remove-orphans"]).await;
            tracing::info!("docker janitor: removed {reason} compose project {name}");
        }
        sweep_dormant_volumes().await;
        sweep_orphaned_images().await;
        run_docker(&["image", "prune", "-f"]).await;
    }
}

/// One best-effort `docker` invocation; `None` when the CLI could not be
/// spawned (daemon gone, docker absent) — every sweep treats that as "nothing
/// to do this tick", exactly like the original janitor's `continue`.
async fn run_docker(args: &[&str]) -> Option<std::process::Output> {
    tokio::process::Command::new("docker")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()
}

/// Every compose project the daemon knows (running, stopped, or config-only)
/// as `(name, status)` pairs; empty when docker did not answer.
async fn compose_projects_with_status() -> Vec<(String, String)> {
    let Some(out) = run_docker(&["compose", "ls", "-a", "--format", "json"]).await else {
        return Vec::new();
    };
    let Ok(list) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|p| {
            let name = p.get("Name")?.as_str()?.to_owned();
            let status = p
                .get("Status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Some((name, status))
        })
        .collect()
}

/// Projects that still own at least one container — running OR stopped —
/// from one `docker ps -a` pass. The evidence the volume sweep's "no container
/// left behind" check needs: a stopped container can be restarted against its
/// volumes, so its project's volumes are not dormant.
async fn projects_with_containers() -> Vec<String> {
    let Some(out) = run_docker(&[
        "ps",
        "-a",
        "--format",
        "{{.Label \"com.docker.compose.project\"}}",
    ])
    .await
    else {
        return Vec::new();
    };
    let mut projects: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    projects.sort_unstable();
    projects.dedup();
    projects
}

/// Every named volume with its compose-project label, from one
/// `docker volume ls --format json` pass (JSONL: one object per line, with
/// `Labels` rendered as a comma-joined `KEY=VALUE` string). Volumes without a
/// compose project label — plain `docker volume create`, anonymous leftovers —
/// come back with `None`.
async fn volumes_with_project_labels() -> Vec<(String, Option<String>)> {
    let Some(out) = run_docker(&["volume", "ls", "--format", "json"]).await else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let volume = serde_json::from_str::<serde_json::Value>(line).ok()?;
            let name = volume.get("Name")?.as_str()?.to_owned();
            let labels = volume.get("Labels").and_then(serde_json::Value::as_str);
            Some((
                name,
                labels.and_then(|l| label_value(l, COMPOSE_PROJECT_LABEL)),
            ))
        })
        .collect()
}

/// The label docker compose stamps on every resource it creates.
const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

/// The value of `key` in docker's comma-joined `KEY=VALUE` label rendering;
/// `None` when the key is absent. Pure, so the label grammar is testable
/// without a daemon.
fn label_value(labels: &str, key: &str) -> Option<String> {
    labels.split(',').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_owned())
    })
}

/// Sweep 2: remove every volume the shared policy admits is dormant —
/// labelled for a reclaimable project that has no container left. Best-effort
/// per volume: a volume that grew a container between the probe and the
/// removal simply fails to remove and stays for the next tick.
async fn sweep_dormant_volumes() {
    let holders = projects_with_containers().await;
    for (volume, project) in volumes_with_project_labels().await {
        if !reclaimable_volume(&volume, project.as_deref(), &holders) {
            continue;
        }
        match run_docker(&["volume", "rm", &volume]).await {
            Some(o) if o.status.success() => {
                tracing::info!("docker janitor: removed dormant volume {volume}");
            }
            Some(o) => tracing::debug!(
                "docker janitor: could not remove dormant volume {volume}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            None => tracing::debug!("docker janitor: docker unavailable, kept volume {volume}"),
        }
    }
}

/// Sweep 3: remove every tagged image the shared policy admits is orphaned —
/// a reclaimable `<project>-<service>` repository whose project is gone and
/// that no container (running or stopped) references. The ancestor probe is
/// the authoritative reference evidence: a repository tag can dangle even
/// while containers keep the image alive by id.
async fn sweep_orphaned_images() {
    let existing: Vec<String> = compose_projects_with_status()
        .await
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let Some(out) = run_docker(&["images", "--format", "{{.Repository}}"]).await else {
        return;
    };
    let mut repositories: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "<none>")
        .map(ToOwned::to_owned)
        .collect();
    repositories.sort_unstable();
    repositories.dedup();
    for repository in repositories {
        let referenced = run_docker(&[
            "ps",
            "-a",
            "-q",
            "--filter",
            &format!("ancestor={repository}"),
        ])
        .await
        // No answer → assume referenced, keep the image.
        .is_some_and(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty());
        if !orphaned_compose_image(&repository, referenced, &existing) {
            continue;
        }
        match run_docker(&["rmi", &repository]).await {
            Some(o) if o.status.success() => {
                tracing::info!("docker janitor: removed orphaned image {repository}");
            }
            Some(o) => tracing::debug!(
                "docker janitor: could not remove orphaned image {repository}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            None => tracing::debug!("docker janitor: docker unavailable, kept image {repository}"),
        }
    }
}

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

#[cfg(test)]
mod janitor_label_tests {
    use super::label_value;

    /// The volume sweep reads the compose project off docker's comma-joined
    /// label rendering — the exact shape `docker volume ls --format json`
    /// emits (verified live: `"Labels":"com.docker.compose.project=oneteam,
    /// com.docker.compose.version=5.1.4"`).
    #[test]
    fn the_compose_project_label_is_read_from_the_joined_rendering() {
        let labels = "com.docker.compose.volume=pgdata,\
                      com.docker.compose.project=cox--dead-preview,\
                      com.docker.compose.version=5.1.4";
        assert_eq!(
            label_value(labels, "com.docker.compose.project"),
            Some("cox--dead-preview".to_owned())
        );
    }

    /// A label-less volume renders an empty or absent Labels string — the
    /// sweep must see no project, never a mistaken one.
    #[test]
    fn an_absent_or_empty_label_renders_no_project() {
        assert_eq!(label_value("", "com.docker.compose.project"), None);
        assert_eq!(
            label_value("com.docker.volume.anonymous=", "com.docker.compose.project"),
            None
        );
    }

    /// KEY=VALUE pairs are split on the FIRST '=' so a value containing '='
    /// survives, and a key only matches exactly — a prefix key must not steal
    /// another pair's value.
    #[test]
    fn keys_match_exactly_and_values_may_contain_equals() {
        assert_eq!(
            label_value("a=b=c,ab=c", "ab"),
            Some("c".to_owned()),
            "the first '=' splits the pair"
        );
        assert_eq!(label_value("abx=c", "ab"), None, "no prefix matching");
    }
}
