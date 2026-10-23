// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! CXA-F420b — the Add-repo wizard's HTTP surface (spec §3–§4).
//!
//! Three doors: list the project's services, scan a candidate repo into one
//! editable proposal, and add a service (probe-first: a green clean-tree
//! build+test earns `active`, anything else lands `needs-setup` with the real
//! error). The scan itself is the pure `service_scan::propose` over a snapshot
//! this adapter takes — walk capped, manifests capped, clone shallow.

use super::*;
use coxagent_application::config::ServiceConfig;
use coxagent_application::service_scan::{propose, RepoSnapshot};
use std::path::{Path as FsPath, PathBuf};

/// GET `/api/projects/:pid/services` — effective services (R1: a legacy
/// config still lists its implicit `main`) plus each one's declared status.
pub(super) async fn list_services_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let cfg = read_project_config(&p.config_path);
    let services = cfg.effective_services(&p.work_dir.to_string_lossy());
    Json(serde_json::json!({ "services": services })).into_response()
}

#[derive(serde::Deserialize)]
pub(super) struct ScanServiceReq {
    /// Git URL to shallow-clone (wins when both are given).
    #[serde(default)]
    pub(super) repo: String,
    /// Existing local path to scan in place.
    #[serde(default)]
    pub(super) path: String,
}

/// POST `/api/projects/:pid/services/scan` — resolve the candidate to a local
/// directory (cloning a URL shallowly next to the project's own codebase),
/// snapshot it, and return the editable proposal. Nothing is written to the
/// project config here: the wizard's card is a draft, not a decision.
pub(super) async fn scan_service_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<ScanServiceReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let repo = req.repo.trim().to_owned();
    let path = req.path.trim().to_owned();
    let dir: PathBuf = if repo.is_empty() {
        if path.is_empty() {
            return bad_request_error("give a git `repo` URL or a local `path`");
        }
        if let Some(refusal) = refused_import_path(Some(&path)) {
            return refusal;
        }
        let dir = PathBuf::from(&path);
        if !dir.is_dir() {
            return bad_request_error("path does not exist or is not a directory");
        }
        dir
    } else {
        if !is_supported_git_url(&repo) {
            return bad_request_error("unsupported git URL (https:// or git@host: only)");
        }
        let name = repo_basename(&repo);
        // The hub-managed clone lives beside the project's own codebase —
        // the same layout the dogfood config uses (`…/cxa/harxes-codebase`).
        let target = p
            .work_dir
            .parent()
            .unwrap_or(&p.work_dir)
            .join(format!("{name}-codebase"));
        if !target.join(".git").is_dir() {
            let out = tokio::process::Command::new("git")
                .args(["clone", "--depth", "1", &repo])
                .arg(&target)
                .output()
                .await;
            match out {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    return bad_request_error(&format!(
                        "clone failed: {}",
                        String::from_utf8_lossy(&o.stderr)
                            .lines()
                            .last()
                            .unwrap_or("git clone error")
                    ));
                }
                Err(e) => return internal_error(&format!("spawn git: {e}")),
            }
        }
        target
    };
    let root_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let snapped = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || snapshot_repo(&dir, root_name)).await
    };
    let snap = match snapped {
        Ok(s) => s,
        Err(e) => return internal_error(&format!("scan task: {e}")),
    };
    let mut proposal = propose(&snap);
    if !repo.is_empty() {
        proposal
            .notes
            .push(format!("cloned (shallow) to {}", dir.display()));
    }
    Json(serde_json::json!({
        "proposal": proposal,
        "repo": repo,
        "work_dir": dir.to_string_lossy(),
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub(super) struct AddServiceReq {
    pub(super) name: String,
    #[serde(default)]
    pub(super) repo: String,
    pub(super) work_dir: String,
    #[serde(default)]
    pub(super) scopes: Vec<String>,
    #[serde(default)]
    pub(super) build_cmd: String,
    #[serde(default)]
    pub(super) test_cmd: String,
    /// Probe-first (spec §3.3): run build+test before the service may be
    /// `active`. Default true; opting out files it as `needs-setup`.
    #[serde(default = "default_true")]
    pub(super) probe: bool,
}

fn default_true() -> bool {
    true
}

/// POST `/api/projects/:pid/services` — append one service to the project
/// config. With `probe` (the default) the clean tree must build and test
/// green to earn `active`; a red probe still files the service, as
/// `needs-setup`, carrying the real error back to the card. New workers are
/// provisioned at hub boot, so the response says so instead of pretending.
pub(super) async fn add_service_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<AddServiceReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let name = req.name.trim().to_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return bad_request_error("service name must be lowercase alphanumeric/dash");
    }
    if let Some(refusal) = refused_import_path(Some(&req.work_dir)) {
        return refusal;
    }
    let work_dir = PathBuf::from(req.work_dir.trim());
    if !work_dir.is_dir() {
        return bad_request_error("work_dir does not exist — scan first (it clones)");
    }
    let mut cfg = read_project_config(&p.config_path);
    let taken: Vec<String> = cfg
        .effective_services(&p.work_dir.to_string_lossy())
        .iter()
        .map(|s| s.name.clone())
        .collect();
    if taken.iter().any(|t| t == &name) {
        return conflict_error(&format!("service '{name}' already exists"));
    }
    // R1: the first explicit services[] entry must keep the legacy default
    // addressable — materialize the implicit `main` before appending, so
    // untagged tickets keep resolving to the same codebase they always did.
    if cfg.services.is_empty() {
        for implicit in cfg.effective_services(&p.work_dir.to_string_lossy()) {
            cfg.services.push(implicit);
        }
    }
    let (status, probe_report) = if req.probe {
        probe_service(&work_dir, &req.build_cmd, &req.test_cmd).await
    } else {
        ("needs-setup".to_owned(), serde_json::json!(null))
    };
    cfg.services.push(ServiceConfig {
        name: name.clone(),
        repo: req.repo.trim().to_owned(),
        work_dir: work_dir.to_string_lossy().into_owned(),
        scopes: req.scopes,
        build_cmd: req.build_cmd,
        test_cmd: req.test_cmd,
        status: status.clone(),
    });
    let text = match serde_json::to_string_pretty(&cfg) {
        Ok(t) => t,
        Err(e) => return internal_error(&e.to_string()),
    };
    if let Err(e) = std::fs::write(&p.config_path, text) {
        return internal_error(&e.to_string());
    }
    Json(serde_json::json!({
        "ok": true,
        "name": name,
        "status": status,
        "probe": probe_report,
        "note": if status == "active" {
            "service active — its worker starts on the next hub restart"
        } else {
            "service filed as needs-setup — fix the probe failure, then re-probe"
        },
    }))
    .into_response()
}

/// Run `build_cmd` then `test_cmd` in `dir`, each bounded; green earns
/// `active`. An empty command is vacuously green (spec: timid defaults —
/// an unknown toolchain can still be added and probed later).
async fn probe_service(
    dir: &FsPath,
    build_cmd: &str,
    test_cmd: &str,
) -> (String, serde_json::Value) {
    for (label, cmd) in [("build", build_cmd), ("test", test_cmd)] {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            continue;
        }
        let run = tokio::time::timeout(
            std::time::Duration::from_secs(900),
            tokio::process::Command::new("sh")
                .args(["-c", cmd])
                .current_dir(dir)
                .output(),
        )
        .await;
        let failed = match &run {
            Ok(Ok(o)) if o.status.success() => None,
            Ok(Ok(o)) => Some(
                String::from_utf8_lossy(if o.stderr.is_empty() {
                    &o.stdout
                } else {
                    &o.stderr
                })
                .chars()
                .rev()
                .take(800)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>(),
            ),
            Ok(Err(e)) => Some(format!("spawn: {e}")),
            Err(_) => Some("timed out after 15 minutes".to_owned()),
        };
        if let Some(err) = failed {
            return (
                "needs-setup".to_owned(),
                serde_json::json!({ "failed": label, "cmd": cmd, "error": err }),
            );
        }
    }
    ("active".to_owned(), serde_json::json!({ "green": true }))
}

/// Parse the project's config off disk; a missing/broken file degrades to
/// defaults exactly like the sibling readers in `projects.rs`.
fn read_project_config(path: &FsPath) -> Config {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default()
}

/// Only the two URL shapes the worker pools themselves clone from.
fn is_supported_git_url(url: &str) -> bool {
    url.starts_with("https://") || (url.starts_with("git@") && url.contains(':'))
}

/// `git@github.com:org/My-Repo.git` / `https://host/org/repo` → `my-repo`.
fn repo_basename(url: &str) -> String {
    let tail = url.rsplit(['/', ':']).next().unwrap_or("service");
    let mut name: String = tail
        .trim_end_matches(".git")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    name.truncate(40);
    let name = name.trim_matches('-').to_owned();
    if name.is_empty() {
        "service".to_owned()
    } else {
        name
    }
}

/// Take the snapshot `service_scan::propose` consumes: a capped walk (depth 3,
/// 2000 entries, skipping VCS/build dirs) plus the root manifests it parses.
fn snapshot_repo(dir: &FsPath, root_name: String) -> RepoSnapshot {
    const SKIP: [&str; 6] = [".git", "node_modules", "target", "dist", ".venv", "vendor"];
    let mut files = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        if files.len() >= 2000 {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if depth < 3 && !SKIP.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if let Ok(rel) = path.strip_prefix(dir) {
                files.push(rel.to_string_lossy().replace('\\', "/"));
                if files.len() >= 2000 {
                    break;
                }
            }
        }
    }
    let manifests = [
        "Cargo.toml",
        "go.work",
        "pnpm-workspace.yaml",
        "package.json",
    ]
    .iter()
    .filter_map(|m| {
        let text = std::fs::read_to_string(dir.join(m)).ok()?;
        Some(((*m).to_owned(), text.chars().take(16_384).collect()))
    })
    .collect();
    RepoSnapshot {
        root_name,
        files,
        manifests,
    }
}

#[derive(serde::Deserialize)]
pub(super) struct ServiceStatusReq {
    /// `active | paused | archived` — the §6 lifecycle verbs. `needs-setup`
    /// is earned by a probe, never set by hand.
    pub(super) status: String,
}

/// POST `/api/projects/:pid/services/:name/status` — pause/resume/archive one
/// service (CXA-F420e, spec §6). Pause keeps tickets and stops assignment;
/// archive is pause plus intent-to-remove (ticket re-homing and worktree
/// cleanup stay with the janitor). The FIRST service is the project's default
/// home for untagged tickets — it can never be paused away.
pub(super) async fn set_service_status_ep(
    State(app): State<AppState>,
    Path((pid, name)): Path<(String, String)>,
    Json(req): Json<ServiceStatusReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let status = req.status.trim().to_lowercase();
    if !matches!(status.as_str(), "active" | "paused" | "archived") {
        return bad_request_error("status must be active, paused or archived");
    }
    let mut cfg = read_project_config(&p.config_path);
    // R1: materialize the implicit single-service form before editing it, so
    // a legacy project can pause what it only implicitly declared.
    if cfg.services.is_empty() {
        cfg.services = cfg.effective_services(&p.work_dir.to_string_lossy());
    }
    let first = cfg.services.first().map(|s| s.name.clone());
    let Some(svc) = cfg.services.iter_mut().find(|s| s.name == name) else {
        return not_found();
    };
    if status != "active" && first.as_deref() == Some(name.as_str()) {
        return bad_request_error(
            "the first service is the default home for untagged tickets — it cannot be \
             paused or archived; reorder services in coxagent.json first",
        );
    }
    let was = svc.status.clone();
    svc.status.clone_from(&status);
    let text = match serde_json::to_string_pretty(&cfg) {
        Ok(t) => t,
        Err(e) => return internal_error(&e.to_string()),
    };
    if let Err(e) = std::fs::write(&p.config_path, text) {
        return internal_error(&e.to_string());
    }
    Json(serde_json::json!({
        "ok": true,
        "name": name,
        "from": was,
        "status": status,
        "note": if status == "active" {
            "worker starts on the next hub restart"
        } else {
            "assignment stops at the next cycle; existing worktrees are cleaned by the janitor"
        },
    }))
    .into_response()
}

/// GET `/api/projects/:pid/services/retag-suggestions` — the CXA-F420e retag
/// pass (spec §4): propose which untagged / default-tagged open tickets look
/// like they belong to another active service. Proposals only; applying is a
/// separate explicit POST with the ticked ids — never silent.
pub(super) async fn retag_suggestions_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let cfg = read_project_config(&p.config_path);
    let services = cfg.effective_services(&p.work_dir.to_string_lossy());
    let Ok(state) = p.store.load().await else {
        return internal_error("state unavailable");
    };
    let views: Vec<coxagent_application::service_retag::TicketView> = state
        .tickets
        .iter()
        .map(|t| coxagent_application::service_retag::TicketView {
            id: t.id().to_string(),
            title: t.title().to_owned(),
            service_tag: t.service_tag().map(ToOwned::to_owned),
            design_files: t
                .design()
                .technical
                .as_ref()
                .map(|d| d.files.clone())
                .unwrap_or_default(),
            terminal: matches!(
                t.status(),
                coxagent_domain::Status::Done
                    | coxagent_domain::Status::Documented
                    | coxagent_domain::Status::Verified
                    | coxagent_domain::Status::Rejected
            ),
        })
        .collect();
    let suggestions = coxagent_application::service_retag::suggest(&views, &services);
    Json(serde_json::json!({ "suggestions": suggestions })).into_response()
}

#[derive(serde::Deserialize)]
pub(super) struct RetagApplyReq {
    /// The ticked suggestions: ticket id → service name.
    pub(super) assignments: Vec<RetagAssignment>,
}
#[derive(serde::Deserialize)]
pub(super) struct RetagAssignment {
    pub(super) ticket: String,
    pub(super) to: String,
}

/// POST `/api/projects/:pid/services/retag` — apply the ticked retags as the
/// human's decision (`Role::User` holds `service_tag` permission). Unknown
/// tickets or inactive target services are reported, not silently skipped.
pub(super) async fn retag_apply_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<RetagApplyReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let cfg = read_project_config(&p.config_path);
    let services = cfg.effective_services(&p.work_dir.to_string_lossy());
    let active: Vec<String> = services
        .iter()
        .filter(|s| s.status == "active")
        .map(|s| s.name.clone())
        .collect();
    let mut applied = Vec::new();
    let mut refused = Vec::new();
    for a in req.assignments {
        if !active.contains(&a.to) {
            refused.push(format!("{}: '{}' is not an active service", a.ticket, a.to));
            continue;
        }
        let (tid, tag) = (a.ticket.clone(), a.to.clone());
        let res = coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), move |s| {
            let t = s
                .tickets
                .iter_mut()
                .find(|t| t.id().as_str() == tid)
                .ok_or_else(|| {
                    coxagent_application::PortError::Backend(format!("unknown ticket {tid}"))
                })?;
            t.set_service_tag(coxagent_domain::Role::User, &tag)
                .map_err(|e| coxagent_application::PortError::Backend(e.to_string()))?;
            Ok(())
        })
        .await;
        match res {
            Ok(()) => applied.push(a.ticket),
            Err(e) => refused.push(format!("{}: {e}", a.ticket)),
        }
    }
    Json(serde_json::json!({ "applied": applied, "refused": refused })).into_response()
}

/// GET `/api/projects/:pid/services/assignments` — the per-user service scope
/// map (CXA-F420f). Missing/empty entry = the member works on every service.
pub(super) async fn service_assignments_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(state) = p.store.load().await else {
        return internal_error("state unavailable");
    };
    Json(serde_json::json!({ "assignments": state.member_services })).into_response()
}

#[derive(serde::Deserialize)]
pub(super) struct AssignServicesReq {
    /// The services this member works on; empty clears the scope (= all).
    pub(super) services: Vec<String>,
}

/// PUT `/api/projects/:pid/services/assignments/:username` — set one member's
/// service scope. Admin/lead only (the shared write gate), names validated
/// against the project's services, empty list clears back to "all".
pub(super) async fn set_service_assignment_ep(
    State(app): State<AppState>,
    Path((pid, username)): Path<(String, String)>,
    Json(req): Json<AssignServicesReq>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let cfg = read_project_config(&p.config_path);
    let known: Vec<String> = cfg
        .effective_services(&p.work_dir.to_string_lossy())
        .iter()
        .map(|s| s.name.clone())
        .collect();
    let mut services: Vec<String> = req
        .services
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    services.sort();
    services.dedup();
    if let Some(bad) = services.iter().find(|s| !known.contains(s)) {
        return bad_request_error(&format!("unknown service '{bad}'"));
    }
    let user = username.trim().to_owned();
    let svc2 = services.clone();
    let res = coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), move |s| {
        if svc2.is_empty() {
            s.member_services.remove(&user);
        } else {
            s.member_services.insert(user.clone(), svc2.clone());
        }
        Ok(())
    })
    .await;
    match res {
        Ok(()) => {
            Json(serde_json::json!({ "ok": true, "username": username, "services": services }))
                .into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Whether `username` may act on a ticket carrying `service_tag` under this
/// project's scope map: no entry (or empty) = full project; an untagged
/// ticket resolves to the FIRST active service, same rule the runners use.
/// Endpoint guard: refuse a ticket write when the signed-in member's service
/// scope excludes the ticket's service. Open mode (no auth) and unscoped
/// members pass untouched; a refusal names the service so the UI can say why.
pub(super) async fn refuse_outside_member_scope(
    app: &AppState,
    headers: &axum::http::HeaderMap,
    p: &crate::server::ProjectHandle,
    service_tag: Option<&str>,
) -> Option<axum::response::Response> {
    app.auth.as_ref()?; // open/local mode — no identities to scope
    let username = resolve_username(app, headers).await;
    let Ok(state) = p.store.load().await else {
        return None; // scope is a courtesy gate — never block on a load fault
    };
    state.member_services.get(&username)?;
    let cfg = read_project_config(&p.config_path);
    let services = cfg.effective_services(&p.work_dir.to_string_lossy());
    if member_may_touch(&state, &services, &username, service_tag) {
        return None;
    }
    let svc = coxagent_application::config::Config::resolve_service(&services, service_tag)
        .map_or_else(|| "?".to_owned(), |s| s.name.clone());
    Some(
        (
            StatusCode::FORBIDDEN,
            format!(
                "ticket belongs to service '{svc}', outside {username}'s assigned services                  (Settings → Services → assignments)"
            ),
        )
            .into_response(),
    )
}

pub(super) fn member_may_touch(
    state: &coxagent_application::ProjectState,
    services: &[coxagent_application::config::ServiceConfig],
    username: &str,
    service_tag: Option<&str>,
) -> bool {
    let Some(mine) = state.member_services.get(username) else {
        return true;
    };
    if mine.is_empty() {
        return true;
    }
    let Some(svc) = coxagent_application::config::Config::resolve_service(services, service_tag)
    else {
        return true; // no active services — nothing to scope by
    };
    mine.iter().any(|s| s == &svc.name)
}
