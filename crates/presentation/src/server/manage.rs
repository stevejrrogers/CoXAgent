// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The super-admin surface: spaces, users, usage, audit, invites, workspace join.

use super::*;

pub(super) async fn audit_push(sink: &Arc<dyn AuditPort>, user: &str, action: String, status: u16) {
    sink.record(AuditRecord {
        at: now_rfc3339(),
        user: user.to_owned(),
        action,
        status,
    })
    .await;
}

/// Assemble the shared [`AppState`] from the registered projects and hub extras.
/// Space budget ENFORCEMENT (not just display): every 5 minutes each space's
/// total spend is compared to its cap; the first breach pauses every runner and
/// registered operator of the space's projects and posts one notice to each
/// project's #agents. Re-arms when the cap is raised above the spend (or the
/// cap is removed) — so topping up the budget lets a Start actually stick.
pub(super) async fn space_budget_watchdog(app: AppState) {
    let mut flagged: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut warned: std::collections::HashSet<String> = std::collections::HashSet::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        let spaces = app.spaces.inner.lock().await.spaces.clone();
        for sp in spaces {
            if sp.budget_usd <= 0.0 {
                flagged.remove(&sp.id);
                warned.remove(&sp.id);
                continue;
            }
            let handles: Vec<ProjectHandle> = {
                let projects = app.projects.read().await;
                sp.projects
                    .iter()
                    .filter_map(|pid| projects.get(pid).cloned())
                    .collect()
            };
            let mut spend = 0.0;
            for p in &handles {
                if let Ok(st) = p.store.load().await {
                    spend += st.spend.total_cost_usd;
                }
            }
            if spend < sp.budget_usd {
                flagged.remove(&sp.id);
            }
            // Early warning ahead of the hard stop below: fires once per
            // approach toward the cap, clears once spend drops back out of
            // the warning band (cap raised, or the hard stop below already
            // took over) so a later crossing can warn again.
            if coxagent_application::policy::approaching_cap(spend, Some(sp.budget_usd), WARN_PCT) {
                if warned.insert(sp.id.clone()) {
                    let msg = format!(
                        "⚠️ BUDGET: space \"{}\" đã đốt ${spend:.2} / cap ${:.2} ({:.0}%) — sắp \
                         chạm mức dừng tự động. Nâng budget (Manage → Edit space) nếu muốn team \
                         tiếp tục chạy liên tục.",
                        sp.name,
                        sp.budget_usd,
                        spend / sp.budget_usd * 100.0
                    );
                    for p in &handles {
                        post_budget_notice(p, &msg, "space budget approaching cap — warned").await;
                    }
                }
            } else {
                warned.remove(&sp.id);
            }
            if spend < sp.budget_usd {
                continue;
            }
            if !flagged.insert(sp.id.clone()) {
                continue; // already enforced for this breach
            }
            tracing::warn!(
                "space {} over budget (${spend:.2} >= ${:.2}) — pausing its agents",
                sp.id,
                sp.budget_usd
            );
            let msg = format!(
                "⛔ BUDGET: space \"{}\" đã đốt ${spend:.2} / cap ${:.2} — toàn bộ agent của \
                 space bị TẠM DỪNG. Super Admin nâng budget (Manage → Edit space) rồi Start lại \
                 để tiếp tục.",
                sp.name, sp.budget_usd
            );
            for p in &handles {
                p.runner.pause();
                if let Ok(workers) = p.store.workers().await {
                    for w in workers {
                        let _ = p.store.set_desired(&w.worker, false).await;
                    }
                }
                post_budget_notice(p, &msg, "space budget cap reached — agents paused").await;
            }
        }
    }
}

pub(super) async fn audit_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let entries = p.store.load().await.map(|s| s.activity).unwrap_or_default();
    let body = serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_owned());
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"coxagent-audit.json\"",
            ),
        ],
        body,
    )
        .into_response()
}

/// Invite a user to a channel, or (with `delegate`) grant them invite rights.
pub(super) async fn channel_invite_ep(
    State(app): State<AppState>,
    Path((pid, cid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChannelMemberReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let actor = resolve_username(&app, &headers).await;
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let result = if req.delegate {
        state.delegate_invite(&cid, &actor, &req.user)
    } else {
        state.invite_to_channel(&cid, &actor, &req.user)
    };
    match result {
        Ok(()) => match p.store.save(&state).await {
            Ok(()) => Json(state.channel(&cid)).into_response(),
            Err(e) => internal_error(&e.to_string()),
        },
        Err(msg) => (StatusCode::FORBIDDEN, msg).into_response(),
    }
}

/// Browse the project codebase: lists the directory at `?path=` (relative to the
/// codebase root, default root). Powers the in-app file browser.
pub(super) async fn workspace_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PathQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let codebase = p.work_dir.clone();
    let dir = if q.path.is_empty() {
        codebase.clone()
    } else {
        match safe_under(&codebase, &q.path) {
            Some(d) if d.is_dir() => d,
            _ => return (StatusCode::BAD_REQUEST, "bad path").into_response(),
        }
    };
    let mut entries: Vec<serde_json::Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if matches!(
                name.as_str(),
                "target" | ".git" | "node_modules" | ".DS_Store"
            ) {
                continue;
            }
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            let size = e.metadata().map_or(0, |m| m.len());
            entries.push(serde_json::json!({ "name": name, "dir": is_dir, "size": size }));
        }
    }
    entries.sort_by(|a, b| {
        let d = |v: &serde_json::Value| {
            v.get("dir")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        d(b).cmp(&d(a)).then_with(|| {
            a.get("name")
                .and_then(serde_json::Value::as_str)
                .cmp(&b.get("name").and_then(serde_json::Value::as_str))
        })
    });
    Json(serde_json::json!({
        "codebase": codebase.display().to_string(),
        "config": p.config_path.display().to_string(),
        "is_git": codebase.join(".git").exists(),
        "path": q.path,
        "entries": entries,
    }))
    .into_response()
}

pub(super) async fn workspace_get_ep(State(app): State<AppState>) -> axum::response::Response {
    let w = app.workspace.inner.lock().await.clone();
    Json(serde_json::json!({
        "name": w.name, "tagline": w.tagline, "accent": w.accent, "conventions": w.conventions,
        "configured": !w.name.trim().is_empty(),
    }))
    .into_response()
}

/// Set the workspace identity (admin — writes are admin-gated by middleware).
pub(super) async fn workspace_put_ep(
    State(app): State<AppState>,
    Json(req): Json<WorkspacePutReq>,
) -> axum::response::Response {
    {
        let mut w = app.workspace.inner.lock().await;
        req.name.trim().clone_into(&mut w.name);
        req.tagline.trim().clone_into(&mut w.tagline);
        req.accent.trim().clone_into(&mut w.accent);
        // Conventions edited on their own screen; only overwrite when provided.
        if let Some(c) = &req.conventions {
            c.trim().clone_into(&mut w.conventions);
        }
        if let Some(d) = req.downloads {
            w.downloads = d;
        }
    }
    app.workspace.save().await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Mint a shareable invite link (admin). Whoever opens it self-registers with
/// the preset role + project membership.
pub(super) async fn invite_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<InviteCreateReq>,
) -> axum::response::Response {
    let by = resolve_username(&app, &headers).await;
    // A normal admin may only invite into projects of THEIR spaces; the super
    // admin is unrestricted. (No spaces defined yet = legacy single-space mode,
    // unrestricted for any admin.)
    if !is_super(&app, &headers).await {
        let mine = spaces_for(&app, &headers).await;
        let has_spaces = !app.spaces.inner.lock().await.spaces.is_empty();
        if has_spaces {
            let allowed: std::collections::HashSet<&String> =
                mine.iter().flat_map(|s| s.projects.iter()).collect();
            if req.projects.iter().any(|p| !allowed.contains(p)) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"error":"you can only invite into your own space's projects"})),
                )
                    .into_response();
            }
        }
    }
    let token = format!(
        "{}{}",
        coxagent_application::state::mint_id(),
        coxagent_application::state::mint_id()
    );
    let invite = Invite {
        token: token.clone(),
        role: req.role.unwrap_or_else(|| "viewer".to_owned()),
        projects: req.projects,
        created_by: by,
        created_at: coxagent_application::state::now_rfc3339(),
        uses_left: req.uses.unwrap_or(5).clamp(1, 100),
    };
    {
        let mut w = app.workspace.inner.lock().await;
        w.invites.push(invite);
    }
    app.workspace.save().await;
    Json(serde_json::json!({ "ok": true, "token": token, "url": format!("/join/{token}") }))
        .into_response()
}

pub(super) async fn invites_list_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Some(auth) = app.auth.clone() {
        let admin = resolve_principal(&auth, &headers)
            .await
            .is_some_and(|u| u.role.can_manage());
        if !admin {
            return (StatusCode::FORBIDDEN, "admin role required").into_response();
        }
    }
    let w = app.workspace.inner.lock().await.clone();
    Json(w.invites).into_response()
}

pub(super) async fn invite_delete_ep(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> axum::response::Response {
    {
        let mut w = app.workspace.inner.lock().await;
        w.invites.retain(|i| i.token != token);
    }
    app.workspace.save().await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// The public join page an invitee lands on: a minimal branded form that
/// registers their account against the invite token.
pub(super) async fn join_page_ep(
    State(app): State<AppState>,
    Path(token): Path<String>,
) -> axum::response::Response {
    let w = app.workspace.inner.lock().await.clone();
    let valid = w
        .invites
        .iter()
        .any(|i| i.token == token && i.uses_left > 0);
    let name = if w.name.trim().is_empty() {
        "CoXAgent".to_owned()
    } else {
        w.name
    };
    let body = if valid {
        format!(
            r#"<h1>Join {n}</h1><p class="sub">Create your account to join the workspace.</p>
<input id="u" placeholder="username" autocomplete="username">
<input id="n" placeholder="display name (optional)">
<input id="p" type="password" placeholder="password" autocomplete="new-password">
<button onclick="go()">Join workspace</button><div id="err"></div>
<script>async function go(){{const r=await fetch('/api/workspace/join',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{token:'{t}',username:u.value.trim(),password:p.value,name:n.value.trim()}})}});if(r.ok)location.href='/';else document.getElementById('err').textContent=(await r.json().catch(()=>({{}}))).error||'could not join';}}
document.addEventListener('keydown',e=>{{if(e.key==='Enter')go()}});</script>"#,
            n = html_escape(&name),
            t = html_escape(&token),
        )
    } else {
        format!(
            "<h1>{}</h1><p class=\"sub\">This invite link is invalid or has been used up. Ask an admin for a new one.</p>",
            html_escape(&name)
        )
    };
    axum::response::Html(format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Join</title>
<style>body{{font-family:-apple-system,system-ui,sans-serif;background:#0b1020;color:#e6ecff;display:grid;place-items:center;min-height:100vh;margin:0}}
.card{{background:#121a30;border:1px solid #24304f;border-radius:16px;padding:34px;width:340px;box-shadow:0 20px 60px rgba(0,0,0,.4)}}
h1{{font-size:20px;margin:0 0 6px}} .sub{{color:#8b98b8;font-size:13px;margin:0 0 18px}}
input{{width:100%;box-sizing:border-box;background:#0b1020;color:#e6ecff;border:1px solid #24304f;border-radius:10px;padding:11px 13px;font-size:14px;margin-bottom:10px}}
button{{width:100%;background:#22d3ee;color:#06202a;border:none;border-radius:10px;padding:12px;font-size:14px;font-weight:700;cursor:pointer}}
#err{{color:#f87171;font-size:12.5px;margin-top:10px;min-height:16px}}</style>
<div class="card">{body}</div>"#
    ))
    .into_response()
}

/// Redeem an invite: create the account with the invite's role + projects,
/// consume one use, and sign the new member straight in.
pub(super) async fn join_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<JoinReq>,
) -> axum::response::Response {
    let Some(auth) = app.auth.clone() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({"error":"auth not configured"})),
        )
            .into_response();
    };
    let username = req.username.trim().to_owned();
    if username.is_empty() || req.password.len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"username and a password (8+ chars) required"})),
        )
            .into_response();
    }
    // Validate + consume one use atomically under the workspace lock.
    let invite = {
        let mut w = app.workspace.inner.lock().await;
        let Some(i) = w
            .invites
            .iter_mut()
            .find(|i| i.token == req.token && i.uses_left > 0)
        else {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error":"invite invalid or used up"})),
            )
                .into_response();
        };
        i.uses_left -= 1;
        let inv = i.clone();
        w.invites.retain(|i| i.uses_left > 0);
        inv
    };
    app.workspace.save().await;
    let role = role_from(Some(invite.role.as_str()));
    if !auth.create_user(&username, &req.password, role).await {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":"username already taken"})),
        )
            .into_response();
    }
    if !req.name.trim().is_empty() {
        auth.update_user(&username, req.name.trim(), "", None).await;
    }
    for pid in &invite.projects {
        auth.assign_project(&username, pid).await;
    }
    audit_push(&app.audit, &username, "joined via invite".to_owned(), 200).await;
    // Sign them straight in (no 2FA on a brand-new account).
    match auth.login(&username, &req.password, None).await {
        coxagent_application::LoginResult::Ok(token) => {
            let cookie = format!(
                "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200{}",
                cookie_secure(&headers)
            );
            (
                [(header::SET_COOKIE, cookie)],
                Json(serde_json::json!({"ok":true})),
            )
                .into_response()
        }
        _ => Json(serde_json::json!({ "ok": true, "login": "manual" })).into_response(),
    }
}

/// The spaces the caller may see: all for a super admin; otherwise the ones
/// they administer or hold a member project in.
pub(super) async fn spaces_for(app: &AppState, headers: &axum::http::HeaderMap) -> Vec<Space> {
    let all = app.spaces.inner.lock().await.spaces.clone();
    if is_super(app, headers).await {
        return all;
    }
    let (me, my_projects) = match app.auth.clone() {
        Some(auth) => resolve_principal(&auth, headers)
            .await
            .map_or((String::new(), Vec::new()), |u| (u.username, u.projects)),
        None => return all,
    };
    all.into_iter()
        .filter(|s| {
            s.admins.iter().any(|a| a.eq_ignore_ascii_case(&me))
                || s.members.iter().any(|m| m.eq_ignore_ascii_case(&me))
                || s.projects.iter().any(|p| my_projects.contains(p))
        })
        .collect()
}

pub(super) async fn spaces_list_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let visible = spaces_for(&app, &headers).await;
    let sup = is_super(&app, &headers).await;
    Json(serde_json::json!({ "spaces": visible, "super": sup })).into_response()
}

/// Validate a space payload against reality: length caps, admins must be real
/// accounts, projects must be registered — a typo must fail loudly, not create
/// silently-broken scoping. Returns the normalized (deduped) lists.
pub(super) async fn validate_space_req(
    app: &AppState,
    req: &SpaceReq,
    exclude_sid: Option<&str>,
) -> Result<(Vec<String>, Vec<String>, Vec<String>), String> {
    if req.name.trim().chars().count() > 60 {
        return Err("name too long (max 60)".into());
    }
    if req.tagline.chars().count() > 160 {
        return Err("tagline too long (max 160)".into());
    }
    if req.admins.len() > 20 || req.projects.len() > 100 || req.members.len() > 200 {
        return Err("too many admins/projects/members".into());
    }
    let known_users: std::collections::HashSet<String> = match app.auth.clone() {
        Some(auth) => auth
            .list_users()
            .await
            .into_iter()
            .map(|u| u.username.to_lowercase())
            .collect(),
        None => std::collections::HashSet::new(),
    };
    let valid_users = |list: &[String]| -> Result<Vec<String>, String> {
        let mut out: Vec<String> = Vec::new();
        for a in list {
            let a = a.trim().to_owned();
            if a.is_empty() || out.contains(&a) {
                continue;
            }
            if !known_users.is_empty() && !known_users.contains(&a.to_lowercase()) {
                return Err(format!("unknown user: {a}"));
            }
            out.push(a);
        }
        Ok(out)
    };
    let admins = valid_users(&req.admins)?;
    let members = valid_users(&req.members)?;
    let known_projects: std::collections::HashSet<String> =
        app.order.read().await.iter().cloned().collect();
    let mut projects = Vec::new();
    for p in &req.projects {
        let p = p.trim().to_owned();
        if p.is_empty() || projects.contains(&p) {
            continue;
        }
        if !known_projects.contains(&p) {
            return Err(format!("unknown project: {p}"));
        }
        projects.push(p);
    }
    // A project belongs to exactly ONE space — claiming one already filed in a
    // different space must fail loudly, not silently double-book it.
    {
        let doc = app.spaces.inner.lock().await;
        for p in &projects {
            if let Some(owner) = doc
                .spaces
                .iter()
                .find(|s| Some(s.id.as_str()) != exclude_sid && s.projects.contains(p))
            {
                return Err(format!("project {p} already belongs to space {}", owner.id));
            }
        }
    }
    Ok((admins, projects, members))
}

/// Adding someone to a space means they can actually WORK there: each member
/// is assigned to every project of the space (additive only — removing a
/// member from the space never auto-revokes project access; that stays an
/// explicit per-project action in Users).
pub(super) async fn assign_space_members(app: &AppState, members: &[String], projects: &[String]) {
    let Some(auth) = app.auth.clone() else { return };
    for m in members {
        for p in projects {
            let _ = auth.assign_project(m, p).await;
        }
    }
}

/// Create a space (super admin only).
pub(super) async fn space_create_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SpaceReq>,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "super admin required").into_response();
    }
    let name = req.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name required").into_response();
    }
    let (admins, projects, members) = match validate_space_req(&app, &req, None).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let id = coxagent_application::state::slugify(&name);
    let by = resolve_username(&app, &headers).await;
    {
        let mut doc = app.spaces.inner.lock().await;
        if doc.spaces.iter().any(|s| s.id == id) {
            return (StatusCode::CONFLICT, "space id already exists").into_response();
        }
        doc.spaces.push(Space {
            id: id.clone(),
            name,
            tagline: req.tagline.trim().to_owned(),
            admins,
            projects: projects.clone(),
            members: members.clone(),
            budget_usd: req.budget_usd.clamp(0.0, 1_000_000.0),
            created_by: by,
            created_at: coxagent_application::state::now_rfc3339(),
        });
    }
    app.spaces.save().await;
    assign_space_members(&app, &members, &projects).await;
    Json(serde_json::json!({ "ok": true, "id": id })).into_response()
}

/// Update a space: super admin, or an admin OF that space.
pub(super) async fn space_update_ep(
    State(app): State<AppState>,
    Path(sid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SpaceReq>,
) -> axum::response::Response {
    let sup = is_super(&app, &headers).await;
    let me = resolve_username(&app, &headers).await;
    let (admins, projects, members) = match validate_space_req(&app, &req, Some(sid.as_str())).await
    {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    {
        let mut doc = app.spaces.inner.lock().await;
        let Some(space) = doc.spaces.iter_mut().find(|s| s.id == sid) else {
            return not_found();
        };
        let allowed = sup || space.admins.iter().any(|a| a.eq_ignore_ascii_case(&me));
        if !allowed {
            return (StatusCode::FORBIDDEN, "not an admin of this space").into_response();
        }
        if !req.name.trim().is_empty() {
            req.name.trim().clone_into(&mut space.name);
        }
        req.tagline.trim().clone_into(&mut space.tagline);
        // Only the super admin reshapes membership/projects/budget of a space.
        if sup {
            space.admins = admins;
            space.projects = projects.clone();
            space.members = members.clone();
            space.budget_usd = req.budget_usd.clamp(0.0, 1_000_000.0);
        }
    }
    app.spaces.save().await;
    if sup {
        assign_space_members(&app, &members, &projects).await;
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

pub(super) async fn space_delete_ep(
    State(app): State<AppState>,
    Path(sid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "super admin required").into_response();
    }
    {
        let mut doc = app.spaces.inner.lock().await;
        doc.spaces.retain(|s| s.id != sid);
    }
    app.spaces.save().await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Deep-dive one space (super admin): every project's health/spend/sprint,
/// and every member with their role and burn — the drill-down behind a card.
pub(super) async fn manage_space_detail_ep(
    State(app): State<AppState>,
    Path(sid): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "super admin required").into_response();
    }
    let Some(space) = app
        .spaces
        .inner
        .lock()
        .await
        .spaces
        .iter()
        .find(|s| s.id == sid)
        .cloned()
    else {
        return not_found();
    };
    let projects_map = app.projects.read().await.clone();
    let mut projects = Vec::new();
    let mut user_spend: HashMap<String, f64> = HashMap::new();
    for pid in &space.projects {
        let Some(p) = projects_map.get(pid) else {
            continue;
        };
        let Ok(st) = p.store.load().await else {
            continue;
        };
        for (op, v) in &st.spend.by_operator {
            let user = op.split('@').next().unwrap_or(op).to_owned();
            *user_spend.entry(user).or_insert(0.0) += v.cost_usd;
        }
        let m = coxagent_application::metrics::compute(&st);
        let workers = p.store.workers().await.unwrap_or_default();
        projects.push(serde_json::json!({
            "id": p.id, "name": p.name, "version": m.version,
            "shipped": m.features_shipped, "in_flight": m.features_in_flight,
            "bugs_open": m.bugs_open, "total_tickets": m.total_tickets,
            "spend": st.spend.total_cost_usd,
            "tokens": st.spend.input_tokens + st.spend.output_tokens,
            "sprint": st.sprint.as_ref().map(|s| serde_json::json!({
                "number": s.number, "goal": s.goal,
                "done": coxagent_application::sprint::done_count(&st),
                "committed": s.committed.len(),
            })),
            "online": workers.iter().map(|w| w.worker.split('@').next().unwrap_or("").to_owned()).collect::<Vec<_>>(),
        }));
    }
    let users = match app.auth.clone() {
        Some(auth) => auth.list_users().await,
        None => Vec::new(),
    };
    let members: Vec<serde_json::Value> = users
        .iter()
        .filter(|u| {
            space
                .admins
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&u.username))
                || u.projects.iter().any(|p| space.projects.contains(p))
        })
        .map(|u| {
            serde_json::json!({
                "username": u.username, "name": u.name, "role": u.role.as_str(),
                "is_space_admin": space.admins.iter().any(|a| a.eq_ignore_ascii_case(&u.username)),
                "spend": user_spend.get(&u.username).copied().unwrap_or(0.0),
            })
        })
        .collect();
    Json(serde_json::json!({ "space": space, "projects": projects, "members": members }))
        .into_response()
}

/// Company-level overview: every project's health + spend + who's online, plus
/// the member directory — the workspace home screen's data.
pub(super) async fn workspace_overview_ep(State(app): State<AppState>) -> axum::response::Response {
    let order = app.order.read().await.clone();
    let projects_map = app.projects.read().await.clone();
    let mut projects = Vec::new();
    for pid in &order {
        let Some(p) = projects_map.get(pid) else {
            continue;
        };
        let Ok(state) = p.store.load().await else {
            continue;
        };
        let m = coxagent_application::metrics::compute(&state);
        let workers = p.store.workers().await.unwrap_or_default();
        // Structural-integrity signal on the hub overview (CXA-F229): every
        // project reports whether its persisted state audits clean, and the
        // quarantine ledger of write-backs the audit already refused.
        let findings = state.audit_structural_integrity();
        let quarantined: Vec<_> = p.store.quarantined().await;
        let quarantined = if quarantined.len() > 10 {
            quarantined[quarantined.len() - 10..].to_vec()
        } else {
            quarantined
        };
        projects.push(serde_json::json!({
            "id": p.id, "name": p.name, "alias": state.alias,
            "version": m.version,
            "shipped": m.features_shipped, "in_flight": m.features_in_flight,
            "bugs_open": m.bugs_open, "total_tickets": m.total_tickets,
            "spend": state.spend.total_cost_usd,
            "sprint": state.sprint.as_ref().map(|s| serde_json::json!({"number": s.number, "goal": s.goal})),
            "online": workers.iter().map(|w| w.worker.split('@').next().unwrap_or("").to_owned()).collect::<Vec<_>>(),
            "integrity": {
                "healthy": findings.is_empty(),
                "findings": findings,
                "quarantined": quarantined,
            },
        }));
    }
    let members = match app.auth.clone() {
        Some(auth) => auth
            .list_users()
            .await
            .into_iter()
            .map(|u| serde_json::json!({"username": u.username, "name": u.name, "role": u.role.as_str(), "projects": u.projects}))
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let w = app.workspace.inner.lock().await.clone();
    Json(serde_json::json!({
        "workspace": {"name": w.name, "tagline": w.tagline, "accent": w.accent},
        "projects": projects, "members": members,
    }))
    .into_response()
}

/// Return the security audit log (admin only, newest first).
pub(super) async fn audit_log_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    // Hub-wide audit trail: Admin/Super only — it records everyone's actions,
    // so ordinary members (can_write) must NOT read it.
    if let Some(auth) = app.auth.clone() {
        let ok = match resolve_principal(&auth, &headers).await {
            Some(u) => matches!(
                u.role,
                coxagent_application::auth::AuthRole::Admin
                    | coxagent_application::auth::AuthRole::Super
            ),
            None => false,
        };
        if !ok {
            return (StatusCode::FORBIDDEN, "admin role required").into_response();
        }
    }
    match app.audit.recent(1000).await {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// The super admin's cross-space overview: every space with its live stats
/// (projects, members, spend, online), plus hub totals and the user directory.
pub(super) async fn manage_overview_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (StatusCode::FORBIDDEN, "super admin required").into_response();
    }
    let order = app.order.read().await.clone();
    let projects_map = app.projects.read().await.clone();
    // Per-project stats once, plus per-user spend rolled up across projects.
    let mut pstats: HashMap<String, (f64, usize, Vec<String>)> = HashMap::new();
    let mut user_spend: HashMap<String, f64> = HashMap::new();
    for pid in &order {
        let Some(p) = projects_map.get(pid) else {
            continue;
        };
        let mut spend = 0.0;
        if let Ok(st) = p.store.load().await {
            spend = st.spend.total_cost_usd;
            for (op, v) in &st.spend.by_operator {
                let user = op.split('@').next().unwrap_or(op).to_owned();
                *user_spend.entry(user).or_insert(0.0) += v.cost_usd;
            }
        }
        let workers = p.store.workers().await.unwrap_or_default();
        let online: Vec<String> = workers
            .iter()
            .map(|w| w.worker.split('@').next().unwrap_or("").to_owned())
            .collect();
        pstats.insert(pid.clone(), (spend, workers.len(), online));
    }
    let users = match app.auth.clone() {
        Some(auth) => auth.list_users().await,
        None => Vec::new(),
    };
    let spaces = app.spaces.inner.lock().await.spaces.clone();
    let mut assigned: std::collections::HashSet<String> = std::collections::HashSet::new();
    let spaces_json: Vec<serde_json::Value> = spaces
        .iter()
        .map(|s| {
            let mut spend = 0.0;
            let mut online: Vec<String> = Vec::new();
            for pid in &s.projects {
                assigned.insert(pid.clone());
                if let Some((sp, _, on)) = pstats.get(pid) {
                    spend += sp;
                    online.extend(on.clone());
                }
            }
            let member_list: Vec<_> = users
                .iter()
                .filter(|u| {
                    s.admins.iter().any(|a| a.eq_ignore_ascii_case(&u.username))
                        || s.members
                            .iter()
                            .any(|m| m.eq_ignore_ascii_case(&u.username))
                        || u.projects.iter().any(|p| s.projects.contains(p))
                })
                .collect();
            let mut roles: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for u in &member_list {
                *roles.entry(u.role.as_str().to_owned()).or_default() += 1;
            }
            serde_json::json!({
                "id": s.id, "name": s.name, "tagline": s.tagline,
                "admins": s.admins, "projects": s.projects,
                "members": member_list.len(), "roles": roles, "spend": spend,
                "budget_usd": s.budget_usd, "online": online,
            })
        })
        .collect();
    let unassigned: Vec<String> = order
        .iter()
        .filter(|p| !assigned.contains(*p))
        .cloned()
        .collect();
    Json(serde_json::json!({
        "spaces": spaces_json,
        "unassigned_projects": unassigned,
        "users": users.iter().map(|u| serde_json::json!({
            "username": u.username, "name": u.name, "role": u.role.as_str(),
            "projects": u.projects,
            "spend": user_spend.get(&u.username).copied().unwrap_or(0.0),
        })).collect::<Vec<_>>(),
        "totals": {
            "projects": order.len(),
            "users": users.len(),
            "spend": pstats.values().map(|(s,_,_)| s).sum::<f64>(),
            "online": pstats.values().map(|(_,n,_)| n).sum::<usize>(),
        },
    }))
    .into_response()
}
