// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Project lifecycle: list, create, import, rename, delete, providers.

use super::*;

pub(super) async fn project_provider(app: &AppState, pid: &str) -> Option<(String, String)> {
    let p = app.project(pid).await?;
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    Some((cfg.git.provider, cfg.git.base_url))
}

/// The configured `owner/name` slug for a project, or `None` when unset.
pub(super) async fn project_repo_slug(app: &AppState, pid: &str) -> Option<String> {
    let p = app.project(pid).await?;
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    Some(cfg.git.repo)
}

/// The forge login this project acts as (empty = the CLI's active account).
pub(super) async fn project_forge_account(app: &AppState, pid: &str) -> Option<String> {
    let p = app.project(pid).await?;
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default();
    Some(cfg.git.account)
}

/// List projects (id, name, alias, version, ticket count) in registration
/// order, then the registered projects that failed to load — flagged `broken`
/// with the reason, so a config error is visible in the dashboard instead of
/// only in the hub log (COX-B043).
pub(super) async fn list_projects(State(app): State<AppState>) -> impl IntoResponse {
    let order = app.order.read().await.clone();
    let mut out = Vec::new();
    for id in &order {
        if let Some(p) = app.project(id).await {
            let (version, tickets) = p.store.load().await.map_or_else(
                |_| ("0.0.0".to_owned(), 0),
                |s| (s.current_version.to_string(), s.tickets.len()),
            );
            out.push(serde_json::json!({
                "id": p.id, "name": p.name, "alias": p.alias,
                "version": version, "tickets": tickets,
                "mode": p.runner.snapshot().mode,
                // Stated on every entry, not just the broken ones: a client
                // that must not select a broken project reads one field.
                "broken": false,
            }));
        }
    }
    out.extend(broken_entries(&app.broken.read().await));
    Json(out)
}

/// Onboard a new project from the dashboard (greenfield, or brownfield import
/// with `existing`, optionally seeded with a `goal`) via the injected factory.
pub(super) async fn create_project(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateProjectReq>,
) -> axum::response::Response {
    // Resolve the target space up front — a bad/unauthorized space must fail
    // BEFORE the project is scaffolded, never leave a half-registered orphan.
    let space_id = req
        .space
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(sid) = space_id {
        let sup = is_super(&app, &headers).await;
        let me = resolve_username(&app, &headers).await;
        let doc = app.spaces.inner.lock().await;
        let Some(space) = doc.spaces.iter().find(|s| s.id == sid) else {
            return (StatusCode::BAD_REQUEST, format!("unknown space: {sid}")).into_response();
        };
        if !sup && !space.admins.iter().any(|a| a.eq_ignore_ascii_case(&me)) {
            return (StatusCode::FORBIDDEN, "not an admin of this space").into_response();
        }
    } else if !app.spaces.inner.lock().await.spaces.is_empty() {
        // Once spaces exist, every project must belong to one — enforced here,
        // not just in the UI, so API/service-account callers can't skip it.
        return (StatusCode::BAD_REQUEST, "space is required").into_response();
    }
    let Some(factory) = app.factory.clone() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "onboarding is only available in hub mode",
        )
            .into_response();
    };
    let name = req.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    // Validate brownfield import path: must be under the hub's workspace root
    // or under /tmp (safe sandbox). Reject paths pointing to system directories.
    if let Some(ref existing) = req.existing {
        if !existing.trim().is_empty() {
            let p = std::path::Path::new(existing.trim());
            // Resolve to absolute canonical path to prevent symlink tricks.
            if let Ok(real) = p.canonicalize() {
                // Allow under /tmp or under $HOME (typical user repos).
                // Block system directories.
                let path_str = real.to_string_lossy();
                // Block if path equals a blocked directory, or if it starts with
                // a blocked directory plus '/', to catch `/private/etc/foo` etc.
                let blocked_prefixes = [
                    "/etc",
                    "/private/etc",
                    "/root",
                    "/var/run",
                    "/var/log",
                    "/usr/lib",
                    "/usr/sbin",
                    "/bin",
                    "/sbin",
                    "/dev",
                    "/proc",
                    "/sys",
                ];
                let blocked = blocked_prefixes.iter().any(|pfx| {
                    path_str == *pfx
                        || path_str.starts_with(pfx)
                            && path_str.as_bytes().get(pfx.len()).copied() == Some(b'/')
                });
                if blocked {
                    return (StatusCode::FORBIDDEN, "cannot import from this path").into_response();
                }
            }
        }
    }
    let handle = match factory(NewProjectReq {
        name,
        alias: req.alias,
        existing: req
            .existing
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from),
        git_url: req.git_url.filter(|s| !s.trim().is_empty()),
        goal: req.goal.filter(|s| !s.trim().is_empty()),
    })
    .await
    {
        Ok(h) => h,
        // CXA-B129: the factory classifies its failures — an expected client
        // conflict (the target workspace already holds tickets) reaches the
        // client as 409, everything else stays a 500.
        Err(e) if e.conflict => return conflict_error(&e.message),
        Err(e) => return internal_error(&e.message),
    };
    let id = handle.id.clone();
    {
        let mut map = app.projects.write().await;
        if map.contains_key(&id) {
            return (StatusCode::CONFLICT, "project id already exists").into_response();
        }
        map.insert(id.clone(), handle);
        app.order.write().await.push(id.clone());
    }
    if let Some(sid) = space_id {
        {
            let mut doc = app.spaces.inner.lock().await;
            if let Some(space) = doc.spaces.iter_mut().find(|s| s.id == sid) {
                if !space.projects.contains(&id) {
                    space.projects.push(id.clone());
                }
            }
        }
        app.spaces.save().await;
    }
    Json(serde_json::json!({ "ok": true, "id": id })).into_response()
}

/// Rename a project: persist the custom display name in its state and update the
/// in-memory handle so the change is live (no restart). Admin-only.
pub(super) async fn rename_project_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<RenameProjectReq>,
) -> axum::response::Response {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return (StatusCode::BAD_REQUEST, "name must be 1–60 chars").into_response();
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    state.display_name = Some(name.to_owned());
    if let Err(e) = p.store.save(&state).await {
        return internal_error(&e.to_string());
    }
    // Reflect the new name in the live handle so list_projects returns it now.
    if let Some(h) = app.projects.write().await.get_mut(&pid) {
        name.clone_into(&mut h.name);
    }
    Json(serde_json::json!({ "ok": true, "name": name })).into_response()
}

/// Delete (deregister) a project: stop its runner, purge its persisted state
/// from the store (shared Postgres row + coordination rows, CXA-B130), remove
/// it from the hub and the registry, and remove the workspace scaffolding from
/// disk. Every step that could resurrect the project under a recreated id is
/// fatal (500); a 200 therefore means the stored state is really gone.
pub(super) async fn delete_project_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    // Deleting a project is destructive — restrict to admins and the lead tier
    // (Director/Manager/*Lead). Everyone else is forbidden. Super (hub-wide
    // owner) is included via `can_manage`.
    if let Some(auth) = &app.auth {
        let allowed = match resolve_principal(auth, &headers).await {
            Some(u) => u.role.can_manage(),
            None => false,
        };
        if !allowed {
            return (
                StatusCode::FORBIDDEN,
                "only an admin or manager may delete a project",
            )
                .into_response();
        }
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    p.runner.stop();
    app.projects.write().await.remove(&pid);
    app.order.write().await.retain(|id| id != &pid);
    // Clean up space references so no dangling project IDs remain.
    {
        let mut sp = app.spaces.inner.lock().await;
        for space in &mut sp.spaces {
            space.projects.retain(|p| p != &pid);
        }
        drop(sp);
        app.spaces.save().await;
    }
    // Purge the project's persisted footprint BEFORE the registry removal:
    // this is the row that resurrected deleted projects (recreating a name
    // whose derived id collided adopted the stale tickets/spend and hit the
    // onboarding "already has tickets" refusal, CXA-B126). A failed purge
    // must NOT answer 200 — the project stays in the registry file, so a
    // restart (or a retry once the store recovers) re-registers it intact
    // and the delete can be attempted again.
    if let Err(e) = p.store.delete().await {
        return internal_error(&e.to_string());
    }
    if let Some(remover) = &app.remover {
        if let Err(e) = remover(pid.clone()).await {
            return internal_error(&e);
        }
    }
    // Clean up the project directory on disk. For imported projects this only
    // removes the CoXAgent workspace scaffolding (state/, coxagent.json, etc.)
    // — never the original imported codebase. Awaited (not fire-and-forget) so
    // a 200 means the scaffolding is actually gone: a surviving directory
    // keeps the derived id occupied and forces `-2` suffixed recreations.
    if let Some(root) = p.config_path.parent() {
        let project_dir = root.to_path_buf();
        let codebase_linked = project_dir.join("codebase.lnk").exists();
        let fs_result = tokio::task::spawn_blocking(move || {
            if codebase_linked {
                // Imported project: only delete CoXAgent scaffolding, not the code.
                let _ = std::fs::remove_file(project_dir.join("codebase.lnk"));
                if let Err(e) = std::fs::remove_dir_all(project_dir.join("state")) {
                    if project_dir.join("state").exists() {
                        return Err(format!("state dir: {e}"));
                    }
                }
                let _ = std::fs::remove_file(project_dir.join("coxagent.json"));
                if let Ok(entries) = std::fs::read_dir(&project_dir) {
                    if entries.count() == 0 {
                        let _ = std::fs::remove_dir(&project_dir);
                    }
                }
            } else {
                // Greenfield: remove the entire project workspace.
                if let Err(e) = std::fs::remove_dir_all(&project_dir) {
                    if project_dir.exists() {
                        return Err(format!("project dir: {e}"));
                    }
                }
            }
            Ok(())
        })
        .await;
        match fs_result {
            Ok(Ok(())) => {}
            Ok(Err(what)) => {
                // The dangerous footprint (the stored state + its mirror) is
                // already purged above; a leftover directory is cosmetically
                // annoying, never a resurrection. Say so loudly anyway.
                tracing::warn!("delete_project {pid}: workspace cleanup failed ({what})");
            }
            Err(e) => {
                tracing::warn!("delete_project {pid}: workspace cleanup task failed: {e}");
            }
        }
        // Also stop + remove the project's Docker compose app, if it was
        // deployed. Detached and best-effort: `docker stop` waits out a grace
        // period we must not spend inside the HTTP request.
        tokio::spawn(async move {
            let container_name = format!("cox-{pid}-codebase-app-1");
            if let Ok(out) = std::process::Command::new("docker")
                .args(["stop", &container_name])
                .output()
            {
                // "No such container" is the COMMON case (most deletions are
                // cleanroom projects that never deployed) — not worth a WARN
                // that reads like something broke.
                let err = String::from_utf8_lossy(&out.stderr);
                if !out.status.success() && !err.contains("No such container") {
                    tracing::warn!("delete_project: docker stop {container_name} failed: {}", err.trim());
                }
            }
            let _ = std::process::Command::new("docker")
                .args(["rm", &container_name])
                .output();
        });
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// The Scrum language configured for a project (English by default).
pub(super) fn project_language(p: &ProjectHandle) -> coxagent_application::config::Language {
    std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .map_or(coxagent_application::config::Language::En, |c| {
            c.workflow.language
        })
}

/// The project brief agents are seeded with (`project_context.md`): its `Goal`
/// section plus the full markdown, so the dashboard can surface what the team
/// is actually building toward.
pub(super) async fn context_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let md = tokio::fs::read_to_string(&p.context_path)
        .await
        .unwrap_or_default();
    Json(serde_json::json!({ "goal": extract_goal(&md), "full": md })).into_response()
}

/// Post an on-demand daily digest (shipped/spend/sprint at a glance) into the
/// project's team chat and return it — the `/digest` slash command.
pub(super) async fn digest_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let now = coxagent_application::state::now_rfc3339();
    let digest = coxagent_application::metrics::digest_markdown(&state, &now);
    drop(state);
    let res = coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        s.post_chat_in(
            "COX",
            &format!("📰 {digest}"),
            coxagent_application::state::AGENTS_CHANNEL,
            Vec::new(),
        );
        Ok(())
    })
    .await;
    match res {
        Ok(()) => Json(serde_json::json!({ "ok": true, "digest": digest })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// GET /api/projects/:pid/milestones/projection (CXA-F249): the forward
/// milestone read model — where each declared milestone stands against the
/// current version and which open work cannot move. A pure read, recomputed
/// from the persisted snapshot on every request, so the forecast updates
/// whenever /state changes; the classification mirrors the release pipeline's
/// own gates, so it can never contradict the next release.
pub(super) async fn milestones_projection_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(state) = p.store.load().await else {
        return internal_error("load failed");
    };
    Json(coxagent_application::milestone_projection::projection_report(&state))
        .into_response()
}
