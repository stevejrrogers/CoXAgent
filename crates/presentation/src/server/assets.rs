// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Files and builds: uploads, media, the code graph, app downloads and updates.

use super::*;

/// What clients need to offer downloads + update notices: the hub's own
/// version, the newest published app version, and per-platform URLs.
pub(super) async fn app_latest_ep(State(app): State<AppState>) -> impl IntoResponse {
    let d = app.workspace.inner.lock().await.downloads.clone();
    Json(serde_json::json!({
        "hub_version": env!("CARGO_PKG_VERSION"),
        "latest_version": d.latest_version,
        "downloads": {
            "macos": d.macos, "windows": d.windows, "linux": d.linux, "ios": d.ios,
        },
        "notes": d.notes,
        "releases_repo": d.releases_repo,
    }))
}

/// Stream a release asset through the hub — the repo may be PRIVATE, so
/// clients can't hit GitHub's download URLs anonymously; the hub's `gh` auth
/// does it server-side and the token never leaves this process. Public path
/// (it serves the installer, same trust as the login page); path ends in the
/// real file extension so the native updater's checks hold.
pub(super) async fn app_download_ep(
    State(app): State<AppState>,
    Path(file): Path<String>,
) -> axum::response::Response {
    let (repo, ver) = {
        let d = &app.workspace.inner.lock().await.downloads;
        (d.releases_repo.trim().to_owned(), d.latest_version.clone())
    };
    if repo.is_empty() || ver.is_empty() {
        return (StatusCode::NOT_FOUND, "no release configured").into_response();
    }
    let want: &[&str] = match file.as_str() {
        "macos.dmg" => &[".dmg"],
        "windows.exe" => &[".exe", ".msi", "windows-x64.zip"],
        "linux.tar.gz" => &["linux-x64.tar.gz", "linux.tar.gz", ".appimage", ".deb"],
        _ => return (StatusCode::NOT_FOUND, "unknown platform").into_response(),
    };
    let tag = format!("v{ver}");
    let Ok(meta) = tokio::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo}/releases/tags/{tag}"),
            "--jq",
            "[.assets[] | {id, name}]",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return internal_error("gh unavailable");
    };
    let assets: Vec<serde_json::Value> = serde_json::from_slice(&meta.stdout).unwrap_or_default();
    let found = assets.iter().find(|a| {
        a.get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|n| {
                let n = n.to_lowercase();
                want.iter().any(|w| n.ends_with(w))
            })
    });
    let Some(asset) = found else {
        return (StatusCode::NOT_FOUND, "no asset for this platform").into_response();
    };
    let id = asset
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let name = asset
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("download")
        .to_owned();
    let Ok(bin) = tokio::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo}/releases/assets/{id}"),
            "-H",
            "Accept: application/octet-stream",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return internal_error("asset fetch failed");
    };
    if !bin.status.success() || bin.stdout.len() < 1024 {
        return internal_error("asset fetch failed");
    }
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        bin.stdout,
    )
        .into_response()
}

/// Poll GitHub Releases (via the `gh` CLI already required for the forge) every
/// 30 minutes and refresh version + per-platform asset URLs — so a release
/// tagged by CI shows up as an update notice in every client, no manual step.
/// Manual URLs in the config win over auto-detected assets.
pub(super) async fn releases_watchdog(app: AppState) {
    loop {
        let repo = app
            .workspace
            .inner
            .lock()
            .await
            .downloads
            .releases_repo
            .clone();
        if !repo.trim().is_empty() {
            let out = tokio::process::Command::new("gh")
                .args([
                    "api",
                    &format!("repos/{}/releases/latest", repo.trim()),
                    "--jq",
                    "{tag: .tag_name, body: .body, assets: [.assets[] | {name, url: .browser_download_url}]}",
                ])
                .stdin(std::process::Stdio::null())
                .output()
                .await;
            if let Ok(o) = out {
                if o.status.success() {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
                        let tag = v
                            .get("tag")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .trim_start_matches('v')
                            .to_owned();
                        let pick = |exts: &[&str]| -> String {
                            v.get("assets")
                                .and_then(serde_json::Value::as_array)
                                .and_then(|a| {
                                    a.iter().find(|x| {
                                        x.get("name")
                                            .and_then(serde_json::Value::as_str)
                                            .is_some_and(|n| {
                                                let n = n.to_lowercase();
                                                exts.iter().any(|e| n.ends_with(e))
                                            })
                                    })
                                })
                                .and_then(|x| x.get("url").and_then(serde_json::Value::as_str))
                                .unwrap_or("")
                                .to_owned()
                        };
                        let (dmg, exe, lin) = (
                            pick(&[".dmg"]),
                            pick(&[".exe", ".msi", "windows-x64.zip"]),
                            pick(&[".appimage", ".deb", "linux-x64.tar.gz", "linux.tar.gz"]),
                        );
                        let mut doc = app.workspace.inner.lock().await;
                        let d = &mut doc.downloads;
                        let changed = !tag.is_empty() && d.latest_version != tag;
                        if !tag.is_empty() {
                            d.latest_version = tag;
                        }
                        d.notes = v
                            .get("body")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(1500)
                            .collect();
                        // Auto-fill only where no manual override exists.
                        if d.macos.is_empty() || d.macos.contains("/releases/") {
                            d.macos = dmg;
                        }
                        if d.windows.is_empty() || d.windows.contains("/releases/") {
                            d.windows = exe;
                        }
                        if d.linux.is_empty() || d.linux.contains("/releases/") {
                            d.linux = lin;
                        }
                        drop(doc);
                        if changed {
                            app.workspace.save().await;
                            tracing::info!("app release refreshed from {repo}");
                        }
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
    }
}

/// Developer tooling the git/deploy flow needs (git, gh, glab, docker) — which
/// are installed, and how to install the rest. Computed at startup and injected.
pub(super) async fn tooling_ep(State(app): State<AppState>) -> impl IntoResponse {
    Json((*app.tooling).clone())
}

/// A compact overview of a loaded code graph (never the full node list).
pub(super) fn codegraph_summary(
    g: &coxagent_application::codegraph::CodeGraph,
) -> serde_json::Value {
    let mut top: Vec<&coxagent_application::codegraph::FileNode> = g.files.iter().collect();
    top.sort_by(|a, b| b.symbols.cmp(&a.symbols).then(b.loc.cmp(&a.loc)));
    let top_files: Vec<serde_json::Value> = top
        .iter()
        .take(40)
        .map(|f| {
            serde_json::json!({
                "path": f.path, "lang": f.lang, "loc": f.loc,
                "symbols": f.symbols, "imports": f.imports.len(),
            })
        })
        .collect();
    serde_json::json!({
        "built": true,
        "built_at": g.built_at,
        "files": g.files.len(),
        "symbols": g.symbols.len(),
        "edges": g.edges.len(),
        "languages": g.languages,
        "top_files": top_files,
    })
}

/// Read the code graph: overview, `?q=` symbol search, or `?map=1` repo map.
pub(super) async fn codegraph_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(query): axum::extract::Query<CodeGraphQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(files) = p.files.clone() else {
        return Json(serde_json::json!({ "built": false })).into_response();
    };
    let Some(g) =
        coxagent_application::codegraph::CodeGraph::load(files.as_ref(), &p.work_dir).await
    else {
        return Json(serde_json::json!({ "built": false })).into_response();
    };
    if let Some(q) = query.q.filter(|q| !q.trim().is_empty()) {
        let results: Vec<_> = g.relevance_search(&q, 60);
        return Json(serde_json::json!({ "built": true, "results": results })).into_response();
    }
    if query.map.unwrap_or(0) == 1 {
        return Json(serde_json::json!({ "built": true, "map": g.repo_map(20_000) }))
            .into_response();
    }
    Json(codegraph_summary(&g)).into_response()
}

/// The internal dependency graph (file → file import edges) for visualisation.
/// Bounded to the most-connected files so the picture stays legible.
pub(super) async fn codegraph_deps_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(files) = p.files.clone() else {
        return Json(serde_json::json!({ "built": false })).into_response();
    };
    let Some(g) =
        coxagent_application::codegraph::CodeGraph::load(files.as_ref(), &p.work_dir).await
    else {
        return Json(serde_json::json!({ "built": false })).into_response();
    };
    let edges = g.resolved_edges();
    // Degree per file (in + out) to pick the interesting nodes.
    let mut degree: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (a, b) in &edges {
        *degree.entry(a.as_str()).or_insert(0) += 1;
        *degree.entry(b.as_str()).or_insert(0) += 1;
    }
    let mut ranked: Vec<(&str, usize)> = degree.into_iter().collect();
    ranked.sort_by_key(|&(_, d)| std::cmp::Reverse(d));
    let keep: std::collections::HashSet<&str> = ranked.iter().take(60).map(|(f, _)| *f).collect();
    let sym_of: std::collections::HashMap<&str, usize> = g
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.symbols))
        .collect();
    let lang_of: std::collections::HashMap<&str, &str> = g
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.lang.as_str()))
        .collect();
    let nodes: Vec<serde_json::Value> = keep
        .iter()
        .map(|f| {
            serde_json::json!({
                "id": f,
                "lang": lang_of.get(f).copied().unwrap_or(""),
                "symbols": sym_of.get(f).copied().unwrap_or(0),
            })
        })
        .collect();
    let links: Vec<serde_json::Value> = edges
        .iter()
        .filter(|(a, b)| keep.contains(a.as_str()) && keep.contains(b.as_str()))
        .map(|(a, b)| serde_json::json!({ "source": a, "target": b }))
        .collect();
    Json(serde_json::json!({
        "built": true, "nodes": nodes, "links": links, "total_files": g.files.len(),
    }))
    .into_response()
}

/// Impact analysis: every whole-word usage of a symbol across the working tree.
pub(super) async fn codegraph_refs_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(query): axum::extract::Query<RefsQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let name = query.name.trim().to_owned();
    if name.len() < 2 {
        return (StatusCode::BAD_REQUEST, "name too short").into_response();
    }
    let Some(files) = p.files.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no workspace access").into_response();
    };
    let refs =
        coxagent_application::codegraph::references(files.as_ref(), &p.work_dir, &name, 200).await;
    let defs = refs.iter().filter(|r| r.is_def).count();
    // Call graph (from the persisted index): who calls this fn, and what it calls.
    let (inbound, outbound) =
        coxagent_application::codegraph::CodeGraph::load(files.as_ref(), &p.work_dir)
            .await
            .map(|g| (g.callers(&name), g.callees(&name)))
            .unwrap_or_default();
    let cg = |v: Vec<(String, String, usize)>| -> Vec<serde_json::Value> {
        v.into_iter()
            .take(100)
            .map(|(label, file, line)| serde_json::json!({ "label": label, "file": file, "line": line }))
            .collect()
    };
    let inbound_n = inbound.len();
    Json(serde_json::json!({
        "name": query.name.trim(),
        "total": refs.len(),
        "defs": defs,
        "uses": refs.len() - defs,
        "refs": refs,
        "callers": cg(inbound),
        "callees": cg(outbound),
        "caller_count": inbound_n,
    }))
    .into_response()
}

/// (Re)build the code graph for a project by indexing its working tree.
pub(super) async fn codegraph_build_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(files) = p.files.clone() else {
        return internal_error("no workspace access configured");
    };
    let work_dir = p.work_dir.clone();
    let g = coxagent_application::codegraph::CodeGraph::index(files.as_ref(), &work_dir).await;
    // save() also writes REPO_MAP.md — one producer, both artifacts.
    match g.save(files.as_ref(), &work_dir).await {
        Ok(()) => Json(codegraph_summary(&g)).into_response(),
        Err(e) => internal_error(&format!("codegraph save failed: {e}")),
    }
}

/// Update the `## Goal` section of the project brief. Admin-only (enforced by
/// `auth_mw`). Note: the running loop captured its context at startup, so an
/// edited goal seeds agent work from the next restart onward.
pub(super) async fn context_update_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<GoalUpdateReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let goal = req.goal.trim();
    if goal.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty goal").into_response();
    }
    if goal.chars().count() > 4000 {
        return (StatusCode::PAYLOAD_TOO_LARGE, "goal too long").into_response();
    }
    let md = tokio::fs::read_to_string(&p.context_path)
        .await
        .unwrap_or_default();
    match tokio::fs::write(&p.context_path, replace_goal(&md, goal)).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Accept a multipart file upload, store it under the project's media dir, and
/// return an [`coxagent_application::Attachment`] the client attaches to a
/// chat/discussion message. Any signed-in user may upload (same as chat).
pub(super) async fn upload_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    mut multipart: axum::extract::Multipart,
) -> axum::response::Response {
    if app.project(&pid).await.is_none() {
        return not_found();
    }
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
        .put(&format!("proj/{pid}/{stored}"), &data, &mime)
        .await
        .is_err()
    {
        return internal_error("write failed");
    }
    let att = serde_json::json!({
        "name": orig,
        "url": format!("/api/projects/{pid}/media/{stored}"),
        "mime": mime,
        "size": data.len(),
    });
    Json(att).into_response()
}

/// Serve an uploaded media file. The filename is a single path segment; a guard
/// rejects any traversal, so only files inside the project's media dir are read.
pub(super) async fn media_ep(
    State(app): State<AppState>,
    Path((pid, file)): Path<(String, String)>,
) -> axum::response::Response {
    if app.project(&pid).await.is_none() {
        return not_found();
    }
    if file.contains('/') || file.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let Ok(bytes) = app.storage.get(&format!("proj/{pid}/{file}")).await else {
        return not_found();
    };
    let mime = mime_of(&file);
    let mut resp = (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, max-age=31536000"),
        ],
        bytes,
    )
        .into_response();
    force_download_if_active_content(&file, &mut resp);
    resp
}

/// Return the text content of one file under the codebase (path-guarded, capped
/// so the browser stays responsive). Powers the in-app file viewer.
pub(super) async fn file_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PathQuery>,
) -> axum::response::Response {
    const MAX: u64 = 512 * 1024;
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(file) = safe_under(&p.work_dir, &q.path).filter(|f| f.is_file()) else {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    };
    if file.metadata().map_or(0, |m| m.len()) > MAX {
        return (StatusCode::PAYLOAD_TOO_LARGE, "file too large to preview").into_response();
    }
    match std::fs::read(&file) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => {
                Json(serde_json::json!({ "path": q.path, "content": text })).into_response()
            }
            Err(_) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, "binary file").into_response(),
        },
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Update a user's profile (name/email) and optionally role. Admin-only.
pub(super) async fn update_user_ep(
    State(app): State<AppState>,
    Path(username): Path<String>,
    Json(req): Json<UpdateUserReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "auth not configured").into_response();
    };
    let role = req.role.as_deref().map(|s| role_from(Some(s)));
    if auth
        .update_user(&username, req.name.trim(), req.email.trim(), role)
        .await
    {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such user").into_response()
    }
}
