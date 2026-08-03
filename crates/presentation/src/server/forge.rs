// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Pull requests as the dashboard sees them: list, review, merge, preview,
//! and the git connection settings.

use super::*;

/// Whether a preview project has a container created longer ago than `ttl`.
/// Docker's own `until` filter does the age arithmetic, so no timestamp
/// parsing (and no timezone bug) of ours stands between a forgotten preview
/// and being reclaimed.
pub(super) async fn preview_is_stale(project: &str, ttl: &str) -> bool {
    let Ok(out) = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-q",
            "--filter",
            &format!("label=com.docker.compose.project={project}"),
            "--filter",
            &format!("until={ttl}"),
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return false;
    };
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

/// The CLI + host env var for a git provider.
pub(super) fn git_cli(provider: &str) -> (&'static str, &'static str) {
    if provider == "gitlab" {
        ("glab", "GITLAB_HOST")
    } else {
        ("gh", "GH_HOST")
    }
}

/// Whether the project's git CLI is signed in, and as whom.
/// End-to-end git connection test: repo? remote? CLI authed? server
/// reachable? PUSH permitted? Each stage is a separate flag so the UI can say
/// exactly what's missing (imported-without-git, no remote, bad token, ...).
pub(super) async fn git_test_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let wd = p.work_dir.clone();
    let is_repo = wd.join(".git").exists();
    let git = |args: &[&str]| {
        let mut c = tokio::process::Command::new("git");
        c.args(args)
            .current_dir(&wd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        c
    };
    let mut remote: Option<String> = None;
    let mut reachable = false;
    let mut push_ok = false;
    let mut detail = String::new();
    if is_repo {
        if let Ok(out) = git(&["remote", "get-url", "origin"]).output().await {
            if out.status.success() {
                let url = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if !url.is_empty() {
                    remote = Some(url);
                }
            }
        }
        if remote.is_some() {
            if let Ok(Ok(out)) = tokio::time::timeout(
                std::time::Duration::from_secs(12),
                git(&["ls-remote", "--heads", "origin"]).output(),
            )
            .await
            {
                reachable = out.status.success();
                if !reachable {
                    detail = String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .last()
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect();
                }
            } else {
                "ls-remote timed out".clone_into(&mut detail);
            }
        }
        if reachable {
            // Dry-run push: proves PUSH permission without writing anything.
            if let Ok(Ok(out)) = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                git(&[
                    "push",
                    "--dry-run",
                    "origin",
                    "HEAD:refs/heads/coxagent-connection-test",
                ])
                .output(),
            )
            .await
            {
                push_ok = out.status.success();
                if !push_ok {
                    detail = String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .last()
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect();
                }
            } else {
                "push --dry-run timed out".clone_into(&mut detail);
            }
        }
    }
    Json(serde_json::json!({
        "repo": is_repo,
        "remote": remote,
        "reachable": reachable,
        "push_ok": push_ok,
        "detail": detail,
    }))
    .into_response()
}

pub(super) async fn git_auth_status_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some((provider, base)) = project_provider(&app, &pid).await else {
        return not_found();
    };
    let (bin, host_env) = git_cli(&provider);
    let host = base
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["auth", "status"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    if !host.is_empty() {
        cmd.env(host_env, &host);
    }
    let (present, authed, account) = match cmd.output().await {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (true, out.status.success(), parse_account(&combined))
        }
        Err(_) => (false, false, None),
    };
    Json(serde_json::json!({
        "tool": bin, "present": present, "authenticated": authed, "account": account,
    }))
    .into_response()
}

/// Sign the project's git CLI in with a user-supplied token, via stdin so the
/// token never appears in the process list; it is not stored or logged by
/// CoXAgent (the CLI keeps it in its own keyring). Admin-only.
pub(super) async fn git_connect_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<ConnectReq>,
) -> axum::response::Response {
    let token = req.token.trim().to_owned();
    if token.is_empty() {
        return (StatusCode::BAD_REQUEST, "token required").into_response();
    }
    let Some((provider, base)) = project_provider(&app, &pid).await else {
        return not_found();
    };
    let (bin, host_env) = git_cli(&provider);
    let is_http = base.trim().starts_with("http://");
    let host = base
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    // Build login args: token on stdin, explicit hostname for self-hosted, and
    // the http protocol when the base URL isn't https (e.g. a local instance).
    let mut login_args: Vec<&str> = vec!["auth", "login"];
    if bin == "glab" {
        login_args.push("--stdin");
    } else {
        login_args.push("--with-token");
    }
    if !host.is_empty() {
        login_args.push("--hostname");
        login_args.push(&host);
    }
    if bin == "glab" && is_http {
        login_args.push("--api-protocol");
        login_args.push("http");
    }
    let host_env_val = if host.is_empty() { "" } else { host_env };
    let (ok, out) = {
        use tokio::io::AsyncWriteExt;
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(&login_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if !host_env_val.is_empty() {
            cmd.env(host_env_val, &host);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                if let Some(mut si) = child.stdin.take() {
                    let _ = si.write_all(token.as_bytes()).await;
                    let _ = si.shutdown().await;
                }
                match child.wait_with_output().await {
                    Ok(o) => (
                        o.status.success(),
                        String::from_utf8_lossy(&o.stderr).trim().to_owned(),
                    ),
                    Err(e) => (false, e.to_string()),
                }
            }
            Err(_) => (false, format!("{bin} not found")),
        }
    };
    if !ok {
        return (StatusCode::BAD_REQUEST, format!("sign-in failed: {out}")).into_response();
    }
    // Verify the token actually authenticates. `glab` stores a token without
    // validating it, so a successful login command is not enough — require the
    // status to resolve an account. If it doesn't, log the bad token back out so
    // we never leave broken credentials behind.
    let (status_ok, status_out) = run_cli_env(bin, &["auth", "status"], host_env_val, &host).await;
    let account = parse_account(&status_out);
    if !status_ok || account.is_none() {
        let host_arg = if host.is_empty() {
            if bin == "glab" {
                "gitlab.com"
            } else {
                "github.com"
            }
        } else {
            host.as_str()
        };
        let _ = run_cli(bin, &["auth", "logout", "--hostname", host_arg]).await;
        return (
            StatusCode::BAD_REQUEST,
            "token was rejected — check the token value and its scopes",
        )
            .into_response();
    }
    Json(serde_json::json!({ "ok": true, "account": account })).into_response()
}

/// The unified diff of one PR (for the in-app review view).
pub(super) async fn pr_diff_ep(
    State(app): State<AppState>,
    Path((pid, num)): Path<(String, u64)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(forge) = &p.forge else {
        return (StatusCode::NOT_IMPLEMENTED, "forge not configured").into_response();
    };
    match forge.pr_diff(num).await {
        Ok(diff) => Json(serde_json::json!({ "diff": diff })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// A review action on a PR (`merge` / `request-changes` / `close`). Requires a
/// reviewer or admin (enforced by [`auth_mw`]).
pub(super) async fn pr_action_ep(
    State(app): State<AppState>,
    Path((pid, num, action)): Path<(String, u64, String)>,
    body: Option<Json<PrActionReq>>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(forge) = &p.forge else {
        return (StatusCode::NOT_IMPLEMENTED, "forge not configured").into_response();
    };
    let comment = body.map(|b| b.0.comment).unwrap_or_default();
    // Preview actions deploy code; they answer with their own payload.
    if action == "preview" || action == "preview-stop" {
        return pr_preview(&p, forge, num, action == "preview").await;
    }
    // Force-merge runs in the background (conflict fix can take minutes).
    // Per-PR in-flight guard: two users clicking Force at once would run two
    // engines in the SAME work_dir, corrupting each other's resolution.
    if action == "force-merge" {
        // Execution-plane routing: with a live runner registered, the job is
        // queued for IT to execute (the control plane never runs engines when
        // it doesn't have to); the runner's 15s poll picks it up. Only when no
        // runner is alive does the hub fall back to executing inline.
        // Registration means "a process that drains jobs": a headless operator
        // beats even while idle, and it polls `drain_jobs` every 15s whether or
        // not it has been Started, so an idle entry is still a safe route.
        let live_runner = p.store.workers().await.is_ok_and(|w| !w.is_empty());
        if live_runner {
            let queued =
                coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
                    if s.jobs.iter().any(|j| {
                        j.kind == "force_merge" && j.args.get("pr") == Some(&serde_json::json!(num))
                    }) {
                        return Ok(()); // already queued — idempotent
                    }
                    s.jobs.push(coxagent_application::state::PendingJob {
                        id: coxagent_application::state::mint_id(),
                        kind: "force_merge".to_owned(),
                        args: serde_json::json!({ "pr": num }),
                        queued_at: coxagent_application::state::now_rfc3339(),
                        queued_by: "web".to_owned(),
                    });
                    Ok(())
                })
                .await;
            if queued.is_ok() {
                return Json(serde_json::json!({ "ok": true, "queued": true })).into_response();
            }
        }
        let key = (pid.clone(), num);
        {
            let mut inflight = force_inflight().lock().await;
            if !inflight.insert(key.clone()) {
                return (
                    StatusCode::CONFLICT,
                    "force-merge for this PR is already running",
                )
                    .into_response();
            }
        }
        let handle = p.clone();
        tokio::spawn(async move {
            force_merge(handle, num).await;
            force_inflight().lock().await.remove(&key);
        });
        return Json(serde_json::json!({ "ok": true, "started": true })).into_response();
    }
    let result = match action.as_str() {
        "merge" => forge.merge_pr(num).await,
        "request-changes" => {
            let c = if comment.trim().is_empty() {
                "Changes requested via CoXAgent review.".to_owned()
            } else {
                comment
            };
            forge.request_changes(num, &c).await
        }
        "close" => forge.close_pr(num).await,
        _ => return (StatusCode::BAD_REQUEST, "unknown action").into_response(),
    };
    match result {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

pub(super) async fn force_merge(p: ProjectHandle, num: u64) {
    let Some(forge) = p.forge.clone() else { return };
    let say = |msg: String| {
        let store = Arc::clone(&p.store);
        async move {
            let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
                s.post_chat_in(
                    "SA",
                    &msg,
                    coxagent_application::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
                Ok(())
            })
            .await;
        }
    };
    let find = || async {
        forge
            .list_open_prs()
            .await
            .ok()
            .and_then(|prs| prs.into_iter().find(|x| x.number == num))
    };
    let Some(pr) = find().await else {
        say(format!("⚡ Force-merge #{num}: PR không còn mở — bỏ qua.")).await;
        return;
    };
    // Blocked? Fix it right now with a DEV engine pass.
    if !pr.mergeable {
        say(format!(
            "⚡ Force-merge #{num}: đang gỡ conflict trên `{}` ngay bây giờ…",
            pr.head
        ))
        .await;
        let request = coxagent_application::ports::outbound::AgentRequest {
            role: coxagent_domain::Role::DevBug,
            system_prompt: coxagent_application::prompts::system_prompt(
                coxagent_application::prompts::DEV,
            ),
            task_prompt: format!(
                "URGENT: a human ordered PR #{num} (branch `{h}`) force-merged. It has merge \
                 conflicts with `{b}`.\n\
                 1. `git fetch origin && git checkout {h} && git pull origin {h}`\n\
                 2. `git merge origin/{b}` and resolve EVERY conflict, preserving both this \
                 branch's fix and what already landed on {b}.\n\
                 3. Run the build/tests to make sure nothing broke.\n\
                 4. `git add -A && git commit -m \"fix: resolve conflicts for #{num}\"` then \
                 `git push origin {h}`.",
                h = pr.head,
                b = pr.base,
            ),
            work_dir: p.work_dir.clone(),
            timeout: std::time::Duration::from_secs(1800),
            escalation_level: 0,
        };
        match p.engine.run(request).await {
            Ok(o) if o.succeeded() => {}
            _ => {
                say(format!(
                    "⚡ Force-merge #{num}: gỡ conflict THẤT BẠI — cần bạn xử lý tay: {}",
                    pr.url
                ))
                .await;
                return;
            }
        }
        // Give the forge a moment to recompute mergeability.
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
    // Merge (retry once after a short wait — mergeability can lag a push).
    for attempt in 0..2u8 {
        match forge.merge_pr(num).await {
            Ok(()) => {
                say(format!("⚡ Force-merge #{num}: ĐÃ MERGE ✓")).await;
                return;
            }
            Err(e) if attempt == 0 => {
                tracing::warn!("force-merge #{num} first attempt: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            }
            Err(e) => {
                say(format!(
                    "⚡ Force-merge #{num}: merge bị từ chối ({e}) — xem PR: {}",
                    pr.url
                ))
                .await;
            }
        }
    }
}

/// Run one git command in `dir`, surfacing stderr on failure.
pub(super) async fn git_pv(dir: &std::path::Path, args: &[&str]) -> Result<(), String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Parse `deploy.host_port` out of a project's raw `coxagent.json` for the
/// preview health-gate probe. A missing/unreadable file, unparseable JSON, a
/// missing `host_port` key, or an explicit `null` all mean "nothing
/// configured" — same contract as `Option<u16>` and
/// `verify_deploy_health`'s no-port pass. Any other JSON value that isn't a
/// valid non-negative `u16` (negative, float, string, bool, out of range) is
/// a corrupt config and must fail the gate rather than being folded into
/// "nothing configured" (COX-B025/COX-B026) — `serde_json::Value::as_u64`
/// returns `None` for all of those just as it does for a genuinely absent
/// field, so the raw JSON value must be inspected instead of going through
/// `as_u64` first.
pub(super) fn parse_preview_host_port(raw_config: &str) -> Result<Option<u16>, ()> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_config) else {
        return Ok(None);
    };
    match value.get("deploy").and_then(|d| d.get("host_port")) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .map(Some)
            .ok_or(()),
    }
}

/// Run the mandatory post-deploy health gate (COX-B004/COX-B009) for a probe
/// port that may be invalid (COX-B025/COX-B026): a corrupt `host_port` fails
/// the gate outright rather than being treated as "nothing configured",
/// which would pass unconditionally and report a dead app as LIVE.
pub(super) async fn run_preview_health_gate(
    deploy: &Arc<dyn coxagent_application::ports::outbound::DeployPort>,
    probe_port: Result<Option<u16>, ()>,
) -> bool {
    match probe_port {
        Ok(port) => coxagent_application::ports::outbound::verify_deploy_health(deploy, port).await,
        Err(()) => false,
    }
}

/// Deploy a PR's branch so the human can SEE the change running before
/// approving (start=true), or tear the preview down and restore main
/// (start=false). The preview runs on the project's app port — one app at a
/// time, honestly labeled — via a git worktree under `<workspace>/.preview/`.
pub(super) async fn pr_preview(
    p: &ProjectHandle,
    forge: &Arc<dyn coxagent_application::ports::outbound::ForgePort>,
    num: u64,
    start: bool,
) -> axum::response::Response {
    let Some(deploy) = &p.deploy else {
        return (StatusCode::NOT_IMPLEMENTED, "deploy not configured").into_response();
    };
    let root = p
        .config_path
        .parent()
        .unwrap_or(&p.config_path)
        .to_path_buf();
    let prev_dir = root.join(".preview").join(num.to_string());
    // The project's published app port, for the "open it" link and the
    // mandatory post-deploy health probe. `probe_port` distinguishes "no
    // port configured" (pass, nothing to probe) from "host_port present but
    // invalid" (fail the gate — see `parse_preview_host_port`).
    let probe_port = std::fs::read_to_string(&p.config_path)
        .ok()
        .map_or(Ok(None), |s| parse_preview_host_port(&s));
    let port = probe_port.unwrap_or_default();
    let chat = |msg: String| {
        let store = Arc::clone(&p.store);
        async move {
            let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
                s.post_chat_in(
                    "COX",
                    &msg,
                    coxagent_application::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
                Ok(())
            })
            .await;
        }
    };
    if start {
        // Resolve the PR's head branch and materialise it in a preview worktree.
        let head = match forge.list_open_prs().await {
            Ok(prs) => match prs.into_iter().find(|x| x.number == num) {
                Some(x) => x.head,
                None => return (StatusCode::NOT_FOUND, "PR not open").into_response(),
            },
            Err(e) => return internal_error(&e.to_string()),
        };
        let refspec = format!("origin/{head}");
        let step = if prev_dir.exists() {
            git_pv(&p.work_dir, &["fetch", "origin", &head])
                .await
                .and(git_pv(&prev_dir, &["reset", "--hard", &refspec]).await)
        } else {
            let _ = std::fs::create_dir_all(prev_dir.parent().unwrap_or(&root));
            git_pv(&p.work_dir, &["fetch", "origin", &head]).await.and(
                git_pv(
                    &p.work_dir,
                    &[
                        "worktree",
                        "add",
                        "--force",
                        &prev_dir.to_string_lossy(),
                        &refspec,
                    ],
                )
                .await,
            )
        };
        if let Err(e) = step {
            return internal_error(&format!("preview checkout: {e}"));
        }
        // Swap: stop the current app, run the PR branch on the app port.
        let _ = deploy.down(&p.work_dir).await;
        match deploy.deploy(&prev_dir).await {
            // Mandatory health gate (COX-B004/COX-B009): a compose exit-0
            // only proves the containers started, not that the app inside
            // bound its port — probe before telling the human it's LIVE.
            Ok(r) if r.success && run_preview_health_gate(deploy, probe_port).await => {
                let url = port.map(|pt| format!("http://localhost:{pt}"));
                chat(format!(
                    "👁 Preview of PR #{num} is LIVE{} — the main build is paused; restore it from the Review tab when done.",
                    url.as_deref().map(|u| format!(" at {u}")).unwrap_or_default()
                ))
                .await;
                Json(serde_json::json!({ "ok": true, "url": url, "summary": r.summary }))
                    .into_response()
            }
            Ok(r) if r.success => internal_error(&format!(
                "preview deploy failed: {} (containers started but the app never bound its port \
                 — health check failed)",
                r.summary
            )),
            Ok(r) => internal_error(&format!("preview deploy failed: {}", r.summary)),
            Err(e) => internal_error(&e.to_string()),
        }
    } else {
        let _ = deploy.down(&prev_dir).await;
        match deploy.deploy(&p.work_dir).await {
            // Same gate on restore: a "restore" that never comes back up on
            // the port must not be reported as a clean restore.
            Ok(r) if r.success && run_preview_health_gate(deploy, probe_port).await => {
                chat(format!(
                    "↩️ Preview of PR #{num} stopped — main build restored."
                ))
                .await;
                Json(serde_json::json!({ "ok": true })).into_response()
            }
            Ok(r) => internal_error(&format!(
                "restore failed: {} (containers started but the app never bound its port — \
                 health check failed)",
                r.summary
            )),
            Err(e) => internal_error(&e.to_string()),
        }
    }
}

/// On-demand SA architecture review: files refactor chores + a PO nudge.
pub(super) async fn architecture_review_ep(
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
    let uc = coxagent_application::use_cases::RunArchitectureAuditUseCase::new(
        Arc::clone(&p.store),
        Arc::clone(&p.engine),
        p.work_dir.clone(),
        cfg.workflow.token_saver,
        cfg.workflow.language,
    )
    .with_files(p.files.clone());
    match uc.execute(current_sprint(&p).await).await {
        Ok(filed) => Json(serde_json::json!({ "ok": true, "filed": filed })).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// On-demand SA merge sweep: merge every green PR in the queue right now
/// (oldest first), report to `#agents`, and return the outcome. Token-free.
pub(super) async fn merge_sweep_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Some(forge) = &p.forge else {
        return (StatusCode::NOT_IMPLEMENTED, "forge not configured").into_response();
    };
    // Target branch + ceremony language from the project config file.
    let cfg = std::fs::read_to_string(&p.config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_default();
    let target = cfg["git"]["target_branch"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| cfg["git"]["default_branch"].as_str())
        .unwrap_or("main")
        .to_owned();
    let vi = cfg["workflow"]["language"].as_str() == Some("vi");
    let require_ci = cfg["git"]["require_ci"].as_bool().unwrap_or(true);
    let out = coxagent_application::use_cases::merge_sweep(
        forge.as_ref(),
        p.store.as_ref(),
        &target,
        vi,
        require_ci,
    )
    .await;
    Json(serde_json::json!({ "ok": true, "merged": out.merged, "skipped": out.skipped }))
        .into_response()
}
