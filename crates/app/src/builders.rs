// Part of the composition root split by concern — see lib.rs.
#![allow(clippy::wildcard_imports)]
//! Wiring: everything that turns config into live adapters — stores, auth,
//! engines, storage, MCP access, and the config self-healing.

use super::*;

/// Build the state store for one project. Backend selection is ordered,
/// REMOTE-first:
///
/// 1. When `COXAGENT_REMOTE_STORE_URL` is set (non-empty), front shared state
///    through this hub's REST gateway (`/store`, RBAC bearer-authed) via a
///    [`RestStateStore`] keyed by `id`. A non-empty `COXAGENT_REMOTE_TOKEN` is
///    presented as the bearer when provided.
/// 2. Else when `COXAGENT_DB_DSN` is set (non-empty), a shared Postgres store
///    keyed by `id` (multi-tenant), with ephemeral leases routed through Redis
///    when `COXAGENT_REDIS_URL` is configured.
/// 3. Else the local JSON file store rooted at `state_dir`.
///
/// This precedence matters: because step 1 runs first, REMOTE wins over DB even
/// if both are somehow set — coordination.json carrying both keys therefore lands
/// on the gateway, not on direct Postgres/Redis access. Setups that configure no
/// remote URL keep today's direct-DB default path unchanged.
///
/// This is the ports adapter swap — use cases never see which backend they got.
pub(crate) async fn make_store(
    id: &str,
    state_dir: &Path,
) -> Result<Arc<AnyStateStore>, Box<dyn std::error::Error>> {
    // Opt-in remote mode : front-end a gateway over REST instead of direct DB .
    if let Ok(url) = std::env::var("COXAGENT_REMOTE_STORE_URL") {
        if !url.is_empty() {
            let cfg = RestConfig {
                base_url: url,
                project_id: id.to_string(),
                // P5a: a runner reaching a gateway that enforces RBAC on /store
                // must present a bearer. Fed by the login-time harvest
                // (AuthPort::auto_issue_personal_token) or set manually.
                token: std::env::var("COXAGENT_REMOTE_TOKEN")
                    .ok()
                    .filter(|t| !t.is_empty()),
            };
            let store = RestStateStore::new(cfg)?;
            tracing::info!("[{id}] state store: REMOTE gateway");
            return Ok(Arc::new(AnyStateStore::Rest(store)));
        }
    }
    match std::env::var("COXAGENT_DB_DSN") {
        Ok(dsn) if !dsn.is_empty() => {
            let mut store = SqlStateStore::connect(&dsn, id).await?;
            // Route the ephemeral leases (leader / stage / worker registry)
            // through Redis when configured — fast TTL keys; Postgres keeps state.
            match std::env::var("COXAGENT_REDIS_URL") {
                Ok(url) if !url.is_empty() => {
                    store = store.with_redis(&url)?;
                    tracing::info!("[{id}] state store: Postgres + Redis coordination");
                }
                _ => tracing::info!("[{id}] state store: Postgres"),
            }
            // First move to Postgres for a project that already has local JSON
            // state: seed it so the existing backlog/history migrates without loss.
            if let Ok(existing) = store.load().await {
                if existing.tickets.is_empty() && existing.comments.is_empty() {
                    if let Ok(js) = JsonStateStore::new(state_dir) {
                        if let Ok(local) = js.load().await {
                            if !local.tickets.is_empty() || !local.comments.is_empty() {
                                store.save(&local).await?;
                                tracing::info!(
                                    "[{id}] seeded Postgres from local JSON ({} tickets, {} comments)",
                                    local.tickets.len(),
                                    local.comments.len()
                                );
                            }
                        }
                    }
                }
            }
            Ok(Arc::new(AnyStateStore::Sql(store)))
        }
        _ => Ok(Arc::new(AnyStateStore::Json(JsonStateStore::new(
            state_dir,
        )?))),
    }
}

/// Load `coxagent.json` from the workspace root (parent of the state dir), or
/// fall back to defaults. Config lives beside the state, written by `onboard`.
/// Load the shared coordination backend (Postgres state DSN + Redis URL) from
/// `<base>/coordination.json` into the environment, unless already set. Lets the
/// Finder-launched app join the distributed backend without env plumbing.
pub fn load_coordination(base: &Path) {
    if std::env::var("COXAGENT_DB_DSN").is_ok_and(|v| !v.is_empty()) {
        return; // an explicit env always wins
    }
    let path = base.join("coordination.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    // Secrets hygiene: this file may carry credentials (DSNs). Clamp it to
    // owner-only and steer real deployments toward env/secret managers —
    // values support `${VAR}` interpolation so the file can stay secret-free.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                tracing::warn!(
                    "coordination.json was group/world-readable ({mode:o}) — tightened to 600. \
                     Prefer COXAGENT_DB_DSN/AUTH_DSN/REDIS_URL env vars (or ${{VAR}} \
                     placeholders in the file) over inline credentials."
                );
            }
        }
    }
    // `${VAR}` placeholders resolve from the environment at load time.
    let text = {
        let mut t = text;
        while let Some(start) = t.find("${") {
            let Some(end_rel) = t[start..].find('}') else {
                break;
            };
            let var = t[start + 2..start + end_rel].to_owned();
            let val = std::env::var(&var).unwrap_or_default();
            t.replace_range(start..=start + end_rel, &val);
        }
        t
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    if let Some(dsn) = v
        .get("db_dsn")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        std::env::set_var("COXAGENT_DB_DSN", dsn);
    }
    if let Some(url) = v
        .get("redis_url")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        std::env::set_var("COXAGENT_REDIS_URL", url);
    }
    if let Some(adsn) = v
        .get("auth_dsn")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        std::env::set_var("COXAGENT_AUTH_DSN", adsn);
    }
    // Remote REST gateway: when present, operators reach shared state through
    // `/store` instead of direct Postgres/Redis. `make_store` reads these and,
    // being checked first, lets REMOTE win over DB even when both are set.
    // Each is applied only when not already set non-empty in the environment —
    // a value handed down by config or login never clobbers one a user exported.
    let set_if_absent = |env_key: &str, value: &str| {
        let already = std::env::var(env_key).is_ok_and(|existing| !existing.trim().is_empty());
        if !already {
            std::env::set_var(env_key, value);
        }
    };
    if let Some(url) = v
        .get("remote_store_url")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        set_if_absent("COXAGENT_REMOTE_STORE_URL", url);
    }
    if let Some(token) = v
        .get("remote_token")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        set_if_absent("COXAGENT_REMOTE_TOKEN", token);
    }

    tracing::info!("coordination config loaded from {}", path.display());
}

/// Canonical machine-local location of the persisted remote-store bearer token.
///
/// One fixed anchor shared by BOTH sides regardless of how each process derived
/// its state dir: the login writer (`server/auth.rs`) and this runner-side reader
/// both resolve it identically so they can never disagree about where the secret
/// lives. Env override `COXAGENT_TOKEN_FILE` wins; else `<home>/CoXAgent/operator.token`.
pub fn operator_token_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COXAGENT_TOKEN_FILE") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    std::env::home_dir().map(|h| h.join("CoXAgent").join("operator.token"))
}

/// Provision a locally-persisted remote-store bearer token for this machine.
///
/// On login, the web dashboard writes this user's personal API token (see
/// `AuthPort::auto_issue_personal_token`) to `<base>/operator.token`, owner-only
/// (0600), so separately-spawned operator processes that have no access to the
/// hub server's process env can still authenticate their `/store` calls. This is
/// the runner-side counterpart: read that file and feed it into
/// `COXAGENT_REMOTE_TOKEN` before [`make_store`] decides on a backend.
///
/// Precedence mirrors [`load_coordination`]: an externally-set non-empty
/// `COXAGENT_REMOTE_TOKEN` always wins — whether handed down by config, exported
/// by the user, or already harvested into this process's env by login — so a
/// caller never clobbers a value someone set on purpose. A missing file, an
/// unreadable one, or one not locked down to this user is skipped silently.
pub fn provision_local_token(_base: &Path) {
    // An explicit token in the environment always wins over a stale file.
    if std::env::var("COXAGENT_REMOTE_TOKEN").is_ok_and(|v| !v.trim().is_empty()) {
        return;
    }
    let Some(path) = operator_token_path() else {
        return;
    };
    // Owner-only: refuse a file whose group/world bits are set — whoever wrote
    // it with looser perms may not be us, so don't hand its secret to /store.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                tracing::warn!(
                    "operator.token is group/world-readable ({mode:o}) — refusing to use it"
                );
                return;
            }
        } else {
            return;
        }
    }
    #[cfg(not(unix))]
    {
        if !path.is_file() {
            return;
        }
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let token = text.trim();
    if token.is_empty() {
        return;
    }
    tracing::debug!("provisioned COXAGENT_REMOTE_TOKEN from {}", path.display());
    std::env::set_var("COXAGENT_REMOTE_TOKEN", token);
}

/// Build one project: store, engine stack, runner (spawned, paused), returned as
/// a `ProjectHandle` the hub server can host alongside others.
#[allow(clippy::too_many_lines)] // one linear wiring pass; splitting hurts readability
pub(crate) async fn build_project(
    id: &str,
    state_dir: &Path,
    work_dir: PathBuf,
    auth: Option<&Arc<dyn coxagent_application::auth::AuthPort>>,
) -> Result<coxagent_presentation::ProjectHandle, Box<dyn std::error::Error>> {
    use coxagent_application::use_cases::{run_forever, RunCycleUseCase, RunnerHandle};

    let store = make_store(id, state_dir).await?;
    // One read settles both the `Config` and the deploy health-gate's host-port
    // probe, so a `deploy.host_port` this project cannot publish fails the gate
    // (COX-B035) or is healed (COX-B042) instead of drifting between the two.
    // A config that does not parse at all fails the project's load (COX-B043).
    let LoadedConfig {
        config,
        host_port_probe,
    } = load_config_with_probe(state_dir)?;
    // `auth` must be the SAME store the hub actually serves /api/mcp with —
    // NOT re-derived from state_dir here. Each project can live under a
    // different workspace root than the hub-wide auth.json (see run_hub's
    // registry), so minting against a re-opened, path-guessed auth store
    // would persist a token the real serving store never loads and every
    // MCP call would 401.
    let mcp = build_mcp_access(&config, auth, id, id).await;
    let (engine, meter) = build_engine(&config, logs_dir(state_dir), mcp.as_ref())?;
    let sleep = std::time::Duration::from_secs(config.workflow.sleep_seconds);

    let recovered = RecoverUseCase::new(Arc::clone(&store)).execute().await?;
    if !recovered.is_empty() {
        tracing::info!("[{id}] recovered {} orphaned claim(s)", recovered.len());
    }

    let loaded = store.load().await.ok();
    let alias = loaded.as_ref().map(|s| s.alias.clone()).unwrap_or_default();
    let custom_name = loaded.as_ref().and_then(|s| s.display_name.clone());
    let mut context =
        std::fs::read_to_string(state_dir.join("project_context.md")).unwrap_or_default();
    // Prepend the company-wide conventions (set once in the Workspace screen) so
    // every agent on every project follows the same house rules.
    let hub_dir = state_dir.parent().and_then(Path::parent);
    if let Some(conv) = hub_dir.and_then(workspace_conventions) {
        if !conv.trim().is_empty() {
            context = format!("## Company conventions (apply to all work)\n{conv}\n\n{context}");
        }
    }
    let webhook = config.workflow.webhook_url.clone();
    // Shared, live-adjustable budget caps — seeded from config, updated by the
    // config API, read by the loop each cycle (so edits apply without a restart).
    let live_budget: coxagent_application::LiveBudget =
        Arc::new(std::sync::Mutex::new(coxagent_application::BudgetCaps {
            lifetime_usd: config.workflow.budget_usd,
            daily_usd: config.policy.daily_budget_usd,
        }));
    // Keep handles for on-demand server actions before they move into the loop.
    let engine_for_handle: Arc<dyn coxagent_application::ports::outbound::AgentEnginePort> =
        engine.clone();
    let work_dir_for_handle = work_dir.clone();
    // Open PRs/MRs on the configured forge when git integration is on. Provider
    // selects the adapter: GitHub via `gh`, GitLab via `glab`.
    let forge: Option<Arc<dyn coxagent_application::ports::outbound::ForgePort>> =
        if config.git.enabled && !config.git.repo.is_empty() {
            let repo = config.git.repo.clone();
            let base = config.git.base_url.clone();
            let wd = work_dir.clone();
            // Which stored login to act as; empty = the CLI's active account.
            let account = config.git.account.clone();
            match config.git.provider.as_str() {
                "gitlab" => Some(Arc::new(coxagent_infrastructure::GlForge::new(
                    repo, base, wd,
                ))),
                "github" => Some(coxagent_infrastructure::github_forge(
                    repo, base, wd, account,
                )),
                _ => None,
            }
        } else {
            None
        };
    let forge_for_handle = forge.clone();
    let concurrency = config.workflow.concurrency.max(1);
    // The agent CLIs on THIS machine travel with the handle so every heartbeat
    // reports them. A hub in a container has none of its own and must be told.
    let handle =
        Arc::new(RunnerHandle::new().with_capabilities(local_caps(&config, &work_dir).await));

    // Hot-reload hook: each runner asks this at its cycle boundary; when
    // coxagent.json changed since last asked, it rebuilds the engine stack from
    // the fresh config so Settings edits apply WITHOUT a hub restart. Each
    // runner gets its own hook (own hash cell) so all of them converge.
    let mk_reloader = {
        let state_dir = state_dir.to_path_buf();
        let mcp = mcp.clone();
        move || {
            let (state_dir, mcp) = (state_dir.clone(), mcp.clone());
            let hash = std::sync::Mutex::new(config_content_hash(&state_dir));
            Arc::new(move || {
                let new = config_content_hash(&state_dir);
                {
                    let mut h = hash.lock().ok()?;
                    if *h == new {
                        return None;
                    }
                    *h = new;
                }
                let reloaded = match load_config_with_probe(&state_dir) {
                    Ok(l) => l.config,
                    Err(e) => {
                        tracing::warn!("config changed but is invalid — keeping previous: {e}");
                        return None;
                    }
                };
                match build_engine(&reloaded, logs_dir(&state_dir), mcp.as_ref()) {
                    Ok((engine, meter)) => Some((reloaded, engine, meter)),
                    Err(e) => {
                        tracing::warn!(
                            "config changed but engine rebuild failed; keeping previous: {e}"
                        );
                        None
                    }
                }
            }) as Arc<dyn Fn() -> _ + Send + Sync>
        }
    };

    // Leader runner: singleton phases (BA, PO, standup, etc.)
    {
        let leader = RunCycleUseCase::new(
            Arc::clone(&store),
            engine.clone(),
            config.clone(),
            work_dir.clone(),
            context.clone(),
        )
        .with_meter(meter.clone())
        .with_live_budget(Arc::clone(&live_budget))
        .with_deploy(Arc::new(DockerComposeDeploy::new()))
        .with_host_port_probe(host_port_probe)
        .with_shot(Some(Arc::new(
            coxagent_infrastructure::screenshot::ChromeScreenshot,
        )))
        .with_probe(Some(Arc::new(coxagent_infrastructure::probe::HttpProbe)))
        // Evidence blobs go where the hub serves media from: S3/MinIO when
        // configured, else the default hub's local blob dir (~/CoXAgent/blobs).
        .with_storage(Some(build_storage().await.unwrap_or_else(|| {
            Arc::new(coxagent_infrastructure::storage::LocalStorage::new(
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_default()
                    .join("CoXAgent")
                    .join("blobs"),
            ))
        })))
        .with_git(Arc::new(coxagent_infrastructure::SystemGit::new()))
        .with_files(Some(Arc::new(
            coxagent_infrastructure::FsWorkspaceFiles::new(),
        )))
        .with_janitor(Some(Arc::new(coxagent_infrastructure::OsProcessJanitor)))
        .with_reloader(mk_reloader());
        let leader = if let Some(ref f) = forge {
            leader.with_forge(Arc::clone(f))
        } else {
            leader
        };
        let leader = leader.with_notifier(build_notifier(Arc::clone(&store), webhook.clone()));
        let leader = if let Some(r) = build_pr_reporter(&config, auth, id, id).await {
            leader.with_reporter(r)
        } else {
            leader
        };
        let wh = Arc::clone(&handle);
        tokio::spawn(async move { run_forever(wh, leader, sleep).await });
    }

    tracing::info!(
        "[{id}] spawning {} worker runner(s) (total {} runners)",
        concurrency.saturating_sub(1),
        concurrency
    );
    for slot in 1..concurrency {
        // Each extra worker runs in its OWN git worktree. Sharing the leader's
        // checkout meant no DEV could ever pass a green-suite DoD — each saw
        // the other's half-written changes (the overnight zero-throughput
        // deadlock). Falls back to the shared tree when this isn't a repo.
        let slot_dir = worktree_at(work_dir.clone(), &format!("{id}-slot-{slot}"));
        let worker = RunCycleUseCase::new(
            Arc::clone(&store),
            engine.clone(),
            config.clone(),
            slot_dir,
            context.clone(),
        )
        .with_meter(meter.clone())
        .with_live_budget(Arc::clone(&live_budget))
        .with_deploy(Arc::new(DockerComposeDeploy::new()))
        .with_host_port_probe(host_port_probe)
        .with_shot(Some(Arc::new(
            coxagent_infrastructure::screenshot::ChromeScreenshot,
        )))
        .with_probe(Some(Arc::new(coxagent_infrastructure::probe::HttpProbe)))
        // Evidence blobs go where the hub serves media from: S3/MinIO when
        // configured, else the default hub's local blob dir (~/CoXAgent/blobs).
        .with_storage(Some(build_storage().await.unwrap_or_else(|| {
            Arc::new(coxagent_infrastructure::storage::LocalStorage::new(
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_default()
                    .join("CoXAgent")
                    .join("blobs"),
            ))
        })))
        .with_git(Arc::new(coxagent_infrastructure::SystemGit::new()))
        .with_files(Some(Arc::new(
            coxagent_infrastructure::FsWorkspaceFiles::new(),
        )))
        .with_janitor(Some(Arc::new(coxagent_infrastructure::OsProcessJanitor)))
        .with_reloader(mk_reloader());
        let worker = if let Some(ref f) = forge {
            worker.with_forge(Arc::clone(f))
        } else {
            worker
        };
        let worker = worker.with_notifier(build_notifier(Arc::clone(&store), webhook.clone()));
        let worker = if let Some(r) = build_pr_reporter(&config, auth, id, id).await {
            worker.with_reporter(r)
        } else {
            worker
        };
        let wh = Arc::clone(&handle);
        tokio::spawn(async move { run_forever(wh, worker, Duration::from_secs(5)).await });
    }

    // Auto-resume this machine's operator if the user left it running last time
    // (per-operator desired state). This restores only THIS user's operator —
    // it never starts anyone else's, so no one's credentials get spent for them.
    if let Ok(op) = std::env::var("COXAGENT_OPERATOR") {
        if !op.is_empty() {
            let operator = format!("{op}@{}", worker_host());
            if matches!(store.get_desired(&operator).await, Ok(Some(true))) {
                handle.set_operator(&op, &worker_host());
                handle.resume();
                tracing::info!("auto-resumed operator {operator} (left running)");
            }
        }
    }

    // Self-upgrade (dogfood CD, opt-in): every 15 min a DETACHED script checks
    // origin/<base> for a commit newer than the deployed hub, builds it in a
    // temp worktree, swaps this very binary (backup kept), restarts, and rolls
    // back if the new hub fails its health check. Detached because a process
    // cannot be trusted to finish replacing itself.
    if config.deploy.self_upgrade {
        let script = work_dir.join("deploy").join("self-upgrade.sh");
        let repo = work_dir.clone();
        let base = config.git.default_branch.clone();
        if let Ok(target) = std::env::current_exe() {
            let port = std::env::var("COXAGENT_PORT")
                .ok()
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(4000);
            let _ = &script; // superseded: the script comes from origin, not the clone
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(900)).await;
                    // Heartbeat: proof the watcher is alive, distinguishable
                    // from "script ran and had nothing to do" (which is
                    // silent by design). The hub's own logs are swallowed by
                    // the app shell, so this file is the only observable.
                    let hb = repo.join(".coxagent-self-upgrade");
                    let _ = std::fs::create_dir_all(&hb);
                    let _ = std::fs::write(
                        hb.join("watcher-heartbeat"),
                        format!("{:?}\n", std::time::SystemTime::now()),
                    );
                    // Run the LATEST script straight from origin/<base> via
                    // `git show` — reading it from the clone was a
                    // chicken-and-egg: a clone that predates the script never
                    // upgrades, and therefore never gets the script.
                    let cmd = "git -C \"$1\" fetch -q origin \"$4\" && \
                         git -C \"$1\" show \"origin/$4:deploy/self-upgrade.sh\" 2>/dev/null \
                         | bash -s -- \"$1\" \"$2\" \"$3\" \"$4\"";
                    let _ = std::process::Command::new("bash")
                        .arg("-c")
                        .arg(cmd)
                        .arg("self-upgrade") // $0
                        .arg(&repo)
                        .arg(&target)
                        .arg(port.to_string())
                        .arg(&base)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                }
            });
            tracing::info!("[{id}] self-upgrade watcher armed (every 15 min)");
        }
    }

    let config_path = state_dir
        .parent()
        .unwrap_or(state_dir)
        .join("coxagent.json");
    Ok(coxagent_presentation::ProjectHandle {
        id: id.to_owned(),
        name: custom_name.unwrap_or_else(|| {
            if alias.is_empty() {
                id.to_owned()
            } else {
                format!("{alias} project")
            }
        }),
        alias,
        store,
        runner: handle,
        config_path,
        engine: engine_for_handle,
        work_dir: work_dir_for_handle,
        budget: live_budget,
        context_path: state_dir.join("project_context.md"),
        forge: forge_for_handle,
        files: Some(std::sync::Arc::new(
            coxagent_infrastructure::FsWorkspaceFiles::new(),
        )),
        deploy: Some(Arc::new(DockerComposeDeploy::new())),
        storage: Some(build_storage().await.unwrap_or_else(|| {
            Arc::new(coxagent_infrastructure::storage::LocalStorage::new(
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_default()
                    .join("CoXAgent")
                    .join("blobs"),
            ))
        })),
    })
}

/// Documentation store: MongoDB when `COXAGENT_MONGO_URL` is set, else `None`
/// (docs fall back to per-project `state.json`).
/// Shared KV store for hub-wide singletons (system chat), backed by Postgres
/// when a state DSN is configured. Falls back to `None` (local file) otherwise.
/// On first use it migrates an existing `system_chat.json` under `hub_dir` into
/// the database so nothing is lost when moving off the local file.
pub(crate) async fn build_syschat_store(
    hub_dir: &Path,
) -> Option<std::sync::Arc<dyn coxagent_application::ports::outbound::KvDocPort>> {
    use coxagent_application::ports::outbound::KvDocPort;
    let dsn = std::env::var("COXAGENT_DB_DSN")
        .ok()
        .filter(|s| !s.is_empty())?;
    match coxagent_infrastructure::PgKvDoc::connect(&dsn).await {
        Ok(store) => {
            // One-time migration: seed the DB from the local file if the DB has
            // no system-chat doc yet but a file exists.
            if matches!(store.load("system_chat").await, Ok(None)) {
                if let Ok(text) = std::fs::read_to_string(hub_dir.join("system_chat.json")) {
                    if !text.trim().is_empty() {
                        match store.save("system_chat", &text).await {
                            Ok(()) => {
                                tracing::info!("system chat: migrated local file into Postgres");
                            }
                            Err(e) => tracing::warn!("system chat migration failed: {e}"),
                        }
                    }
                }
            }
            tracing::info!("system chat store: Postgres");
            Some(std::sync::Arc::new(store))
        }
        Err(e) => {
            tracing::warn!("Postgres KV configured but unavailable ({e}); using local file");
            None
        }
    }
}

pub(crate) async fn build_doc_store(
) -> Option<std::sync::Arc<dyn coxagent_application::ports::outbound::DocStorePort>> {
    match coxagent_infrastructure::MongoDocStore::from_env().await {
        Ok(Some(store)) => {
            tracing::info!("documentation store: MongoDB");
            Some(std::sync::Arc::new(store))
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("MongoDB configured but unreachable ({e}); using per-project state");
            None
        }
    }
}

/// Blob storage backend: S3/MinIO when `COXAGENT_S3_*` is configured, else the
/// presentation layer's local-disk default.
pub(crate) async fn build_storage(
) -> Option<std::sync::Arc<dyn coxagent_application::ports::outbound::StoragePort>> {
    let s3 = coxagent_infrastructure::S3Storage::from_env()?;
    match s3.ensure_bucket().await {
        Ok(()) => tracing::info!("blob storage: S3/MinIO"),
        Err(e) => tracing::warn!("S3 bucket check failed ({e}); uploads may fail"),
    }
    Some(std::sync::Arc::new(s3))
}

/// Engine CLIs found on PATH, as `(name, path)` for the dashboard.
/// The `provider/model` pairs this machine's opencode can reach — reported to
/// the hub so the settings dropdown can offer a user's CUSTOM providers, which
/// no built-in list knows and no container can detect.
pub(crate) fn detected_models() -> Vec<String> {
    coxagent_infrastructure::discover_opencode_models()
}

/// Everything THIS machine can do, for the worker registry: the agent CLIs on
/// its PATH, the models its opencode reaches, and whether its git and forge
/// credentials actually work.
///
/// All three are unknowable to a hub served from a container — it has no CLI,
/// no ssh key and no checkout. The machine that has them is the only one that
/// can answer, so it answers here and publishes through the heartbeat.
pub(crate) async fn local_caps(
    config: &Config,
    work_dir: &Path,
) -> coxagent_application::ports::outbound::WorkerCaps {
    coxagent_application::ports::outbound::WorkerCaps {
        engines: detected_engines().into_iter().map(|(n, _)| n).collect(),
        models: detected_models(),
        tooling: Some(detected_tooling()),
        git: if config.git.enabled && !config.git.repo.is_empty() {
            Some(
                coxagent_infrastructure::probe_git_access(
                    &config.git.repo,
                    &config.git.account,
                    work_dir,
                )
                .await,
            )
        } else {
            None
        },
    }
}

pub(crate) fn detected_engines() -> Vec<(String, String)> {
    discover()
        .into_iter()
        .map(|d| (d.kind.as_binary().to_owned(), d.path.display().to_string()))
        .collect()
}

/// Developer tooling status (git/gh/glab/docker) serialized for the dashboard.
pub(crate) fn detected_tooling() -> serde_json::Value {
    serde_json::to_value(coxagent_infrastructure::discover_tooling()).unwrap_or_default()
}

/// Build the security-audit sink: Postgres when `COXAGENT_DB_DSN` is set (the
/// trail then survives restarts and is shared across the hub), else in memory.
pub(crate) async fn build_audit() -> Arc<dyn coxagent_application::ports::outbound::AuditPort> {
    use coxagent_infrastructure::{MemoryAuditSink, SqlAuditSink};
    // Optional compliance retention: prune audit rows older than N days.
    let retention_days = std::env::var("COXAGENT_AUDIT_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok());
    if let Ok(dsn) = std::env::var("COXAGENT_DB_DSN") {
        if !dsn.is_empty() {
            match SqlAuditSink::connect(&dsn, retention_days).await {
                Ok(sink) => {
                    tracing::info!("audit sink: Postgres (retention: {retention_days:?} days)");
                    return Arc::new(sink);
                }
                Err(e) => tracing::warn!("audit sink: Postgres unavailable ({e}); using memory"),
            }
        }
    }
    Arc::new(MemoryAuditSink::default())
}

/// Wire RBAC. An admin is (re-)provisioned from the `COXAGENT_ADMIN_USER` /
/// `COXAGENT_ADMIN_PASSWORD` env vars into `auth.json` under `base` — the env is
/// authoritative, so setting it always makes that password work even if a stale
/// file exists. With no file and no env, the server runs open (no login).
pub(crate) async fn build_auth(
    base: &Path,
) -> Result<Option<Arc<dyn coxagent_application::auth::AuthPort>>, Box<dyn std::error::Error>> {
    use coxagent_application::auth::AuthPort;
    use coxagent_infrastructure::{FileAuthService, SqlAuthService};
    let admin = match (
        std::env::var("COXAGENT_ADMIN_USER"),
        std::env::var("COXAGENT_ADMIN_PASSWORD"),
    ) {
        (Ok(user), Ok(pass)) => Some((user, pass)),
        _ => None,
    };

    // Server mode: accounts / membership / tokens live in Postgres (shared) only
    // when an explicit auth DSN is set — kept SEPARATE from the state DSN so a
    // project can move its state to Postgres while keeping the local account file
    // (no forced re-login when going distributed on one host).
    if let Ok(dsn) = std::env::var("COXAGENT_AUTH_DSN") {
        let mut svc = SqlAuthService::connect(&dsn).await?;
        // Sessions are ephemeral TTL data — store them in Redis (native expiry)
        // when available, else they fall back to the Postgres auth_sessions table.
        if let Ok(url) = std::env::var("COXAGENT_REDIS_URL") {
            if !url.is_empty() {
                svc = svc.with_redis(&url)?;
            }
        }
        svc.restore_sessions().await;
        if let Some((user, pass)) = &admin {
            svc.bootstrap_admin(user, pass).await?;
            tracing::info!("admin '{user}' provisioned in Postgres");
        }
        if svc.list_users().await.is_empty() {
            tracing::info!("no auth configured — running open");
            return Ok(None);
        }
        tracing::info!("RBAC enabled (Postgres auth store)");
        return Ok(Some(Arc::new(svc)));
    }

    // Local mode: a JSON account file next to the workspace.
    let auth_path = FileAuthService::default_path(base);
    if let Some((user, pass)) = &admin {
        FileAuthService::bootstrap_admin(&auth_path, user, pass)?;
        tracing::info!("admin '{user}' provisioned from environment");
    }
    let svc = FileAuthService::open(&auth_path)?;
    if svc.has_users() {
        tracing::info!("RBAC enabled ({} account file)", auth_path.display());
        Ok(Some(Arc::new(svc)))
    } else {
        tracing::info!(
            "no auth configured — running open (set COXAGENT_ADMIN_USER/PASSWORD to enable)"
        );
        Ok(None)
    }
}

/// A minted API token, scoped read-only, that this operator's spawned agent
/// CLIs (claude/opencode) use to call the hub's own `/api/mcp` endpoint —
/// letting agents query the code graph live instead of only through the
/// static prompt text. Labeled `internal:mcp:<identity>` and filtered out of
/// the admin dashboard's token list (`list_tokens_ep`) — it's plumbing, not a
/// credential for a human to manage. Re-minted on every startup (the previous
/// one for this identity is revoked first, keeping auth.json from
/// accumulating dead entries across restarts); `None` when auth is disabled
/// (`/api/mcp` accepts unauthenticated loopback calls in that mode) or
/// minting fails.
///
/// Persisted (as a hash, like any other token) rather than held purely in
/// memory: the operator that spawns the agent CLI and the hub that serves
/// `/api/mcp` are not always the same OS process (see `Command::Run` vs
/// `Command::Serve`), so the credential has to be checkable by whichever
/// process is actually serving the request.
pub(crate) async fn ensure_internal_mcp_token(
    auth: &Arc<dyn coxagent_application::auth::AuthPort>,
    identity: &str,
) -> Option<String> {
    use coxagent_application::auth::AuthRole;
    let label = format!("internal:mcp:{identity}");
    auth.revoke_token(&label).await; // drop any stale token from a prior run
                                     // NOT AuthRole::Viewer: `auth_mw` (server.rs) gates every POST as a write
                                     // unless the path is explicitly exempted, and /api/mcp isn't (it's the
                                     // single JSON-RPC endpoint for both reads like search_symbols and writes
                                     // like report_blocker, so the middleware can't tell them apart from the
                                     // HTTP verb alone). Viewer.can_write() is false, so a Viewer-scoped
                                     // token would get 403'd on every call, including read-only ones. `Be` is
                                     // the lowest member-tier role that still satisfies can_write() — chosen
                                     // arbitrarily among the member tier, since none of them map naturally to
                                     // "the agent working this project" and MCP tool access doesn't
                                     // distinguish between member sub-roles.
    let secret = auth.create_token(&label, AuthRole::Be).await;
    if secret.is_none() {
        tracing::warn!("could not mint internal MCP token for {identity}");
    }
    secret
}

/// Build this run's loopback [`McpAccess`] for `project` (its id, as known to
/// the hub — goes on every MCP tool call), when the hub is expected to be
/// reachable on `config.deploy.host_port`. `token_identity` namespaces the
/// internal token label (see [`ensure_internal_mcp_token`]) — pass something
/// that's unique to the *process* minting it (e.g. including the operator
/// name), not just the project, so two operators working the same project
/// concurrently don't revoke each other's freshly minted token.
pub(crate) async fn build_mcp_access(
    config: &Config,
    auth: Option<&Arc<dyn coxagent_application::auth::AuthPort>>,
    project: &str,
    token_identity: &str,
) -> Option<coxagent_infrastructure::engine::McpAccess> {
    let port = config.deploy.host_port.unwrap_or(4000);
    let token = match auth {
        Some(auth) => ensure_internal_mcp_token(auth, token_identity).await,
        None => None, // open mode: /api/mcp accepts unauthenticated loopback calls
    };
    Some(coxagent_infrastructure::engine::McpAccess {
        url: format!("http://127.0.0.1:{port}/api/mcp"),
        token,
        project: project.to_owned(),
    })
}

/// Mint (and revoke the previous) an internal bearer token the runner presents
/// when reporting PR/review activity to the hub. Same idea as the MCP token —
/// plumbing, not a human credential — but labelled separately so the two kinds
/// of internal traffic stay distinguishable in the token store. `None` when
/// auth is disabled (loopback then needs no token).
pub(crate) async fn ensure_internal_pr_token(
    auth: &Arc<dyn coxagent_application::auth::AuthPort>,
    identity: &str,
) -> Option<String> {
    use coxagent_application::auth::AuthRole;
    let label = format!("internal:pr-report:{identity}");
    auth.revoke_token(&label).await; // drop stale before re-minting
    let secret = auth.create_token(&label, AuthRole::Be).await;
    if secret.is_none() {
        tracing::warn!("could not mint internal PR-report token for {identity}");
    }
    secret
}

/// Build the runner's PR reporter when the project opts into reporting (sets
/// `git.server_url`). The runner presents an internally-minted bearer token —
/// no forge secret or user-facing token in config — and posts PR/review events
/// to the hub over HTTP, so the web dashboard on any machine can show them.
pub(crate) async fn build_pr_reporter(
    config: &Config,
    auth: Option<&Arc<dyn coxagent_application::auth::AuthPort>>,
    project: &str,
    identity: &str,
) -> Option<Arc<dyn coxagent_application::ports::outbound::PrReporterPort>> {
    let server_url = config.git.server_url.trim();
    if server_url.is_empty() {
        return None;
    }
    let token = match auth {
        Some(auth) => ensure_internal_pr_token(auth, identity).await?,
        None => return None, // open mode still needs a hub URL + token to report
    };
    Some(Arc::new(coxagent_infrastructure::HttpPrReporter::new(
        project, server_url, token,
    )))
}

/// The event notifier for a runner: always the project's own team chat (with a
/// native push via the app's chat-notification path), plus an external webhook
/// when one is configured.
pub(crate) fn build_notifier(
    store: Arc<AnyStateStore>,
    webhook: Option<String>,
) -> Arc<dyn coxagent_application::ports::outbound::NotifierPort> {
    use coxagent_application::ports::outbound::{ChatNotifier, FanoutNotifier, NotifierPort};
    let mut sinks: Vec<Arc<dyn NotifierPort>> = vec![Arc::new(ChatNotifier::new(store))];
    if let Some(url) = webhook.filter(|u| !u.is_empty()) {
        sinks.push(Arc::new(WebhookNotifier::new(url)));
    }
    Arc::new(FanoutNotifier(sinks))
}

/// Who holds `port`, as `name (pid)`, for the message a person needs when the
/// hub had to move. Best-effort: an unavailable `lsof` just means less detail.
pub(crate) fn port_holder(port: u16) -> Option<String> {
    let out = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{port}")])
        .output()
        .ok()?;
    let pid = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .to_owned();
    if pid.is_empty() {
        return None;
    }
    let name = std::process::Command::new("ps")
        .args(["-p", &pid, "-o", "comm="])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "an unknown process".to_owned());
    Some(format!("{name} (pid {pid})"))
}

#[cfg(test)]
mod builders_tests {
    use super::{load_coordination, provision_local_token};
    use std::path::PathBuf;

    /// These tests read/write process-global env vars; serialize them so they
    /// cannot clobber one another's values when Rust runs them on many threads.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn provision_local_token_sets_env_and_respects_preexisting() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Point the canonical location at a throwaway path so we never touch a
        // real ~/CoXAgent token while testing.

        let dir = tempfile::TempDir::new().unwrap();
        let base: PathBuf = dir.path().to_path_buf();
        std::env::set_var("COXAGENT_TOKEN_FILE", base.join("operator.token"));
        std::fs::write(base.join("operator.token"), "secret-abc\n").unwrap();
        // Owner-only (0600) — see the permission test for the mode check.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                base.join("operator.token"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }

        // Case A — fresh env: the file's token lands on COXAGENT_REMOTE_TOKEN.
        std::env::remove_var("COXAGENT_REMOTE_TOKEN");
        provision_local_token(&base);
        assert_eq!(
            std::env::var("COXAGENT_REMOTE_TOKEN").unwrap(),
            "secret-abc",
            "a persisted operator token should populate COXAGENT_REMOTE_TOKEN"
        );

        // Case B — an externally-set non-empty value must NOT be overwritten.
        std::env::set_var("COXAGENT_REMOTE_TOKEN", "external-token");
        provision_local_token(&base);
        assert_eq!(
            std::env::var("COXAGENT_REMOTE_TOKEN").unwrap(),
            "external-token",
            "a pre-existing non-empty COXAGENT_REMOTE_TOKEN must win over the file"
        );

        // Leave no trace behind for parallel tests.
        std::env::remove_var("COXAGENT_REMOTE_TOKEN");
    }

    #[test]
    fn provision_local_token_skips_absent_empty_and_locked_files() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let base: PathBuf = dir.path().to_path_buf();
        // Isolate the canonical location from any real ~/CoXAgent token.
        std::env::set_var("COXAGENT_TOKEN_FILE", base.join("operator.token"));

        // Absent file: no env set, no panic.
        std::env::remove_var("COXAGENT_REMOTE_TOKEN");
        provision_local_token(&base);
        assert_eq!(std::env::var_os("COXAGENT_REMOTE_TOKEN"), None);

        // Empty (whitespace-only) file: skipped gracefully.
        std::fs::write(base.join("operator.token"), "   \n").unwrap();
        provision_local_token(&base);
        assert_eq!(std::env::var_os("COXAGENT_REMOTE_TOKEN"), None);

        // Group/world-readable file is refused — we won't hand a loose secret
        // to /store on behalf of whoever wrote it. On non-unix there's no mode
        // check, so guard the assertion.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                base.join("operator.token"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        provision_local_token(&base);
        assert_eq!(std::env::var_os("COXAGENT_REMOTE_TOKEN"), None);
    }

    #[test]
    fn load_coordination_sets_all_five_keys_and_respects_preexisting() {
        let _guard = ENV_LOCK.lock().unwrap();
        let cases = [
            ("db_dsn", "COXAGENT_DB_DSN", "postgres://db"),
            ("redis_url", "COXAGENT_REDIS_URL", "redis://cache"),
            ("auth_dsn", "COXAGENT_AUTH_DSN", "postgres://auth"),
            (
                "remote_store_url",
                "COXAGENT_REMOTE_STORE_URL",
                "http://127.0.0.1:8101",
            ),
            ("remote_token", "COXAGENT_REMOTE_TOKEN", "t0k"),
        ];
        for (_, env_key, _) in &cases {
            std::env::remove_var(env_key);
        }

        let dir = tempfile::TempDir::new().unwrap();
        let base: PathBuf = dir.path().to_path_buf();
        let json: Vec<String> = cases
            .iter()
            .map(|(k, _, v)| format!("\"{k}\": \"{v}\""))
            .collect();
        std::fs::write(
            base.join("coordination.json"),
            format!("{{{}}}", json.join(",")),
        )
        .unwrap();

        // Case A — fresh environment: all five keys land on their env vars,
        // including the new remote-store pair.
        load_coordination(&base);
        for (_, env_key, expected) in &cases {
            assert_eq!(
                std::env::var(env_key).ok(),
                Some((*expected).to_string()),
                "{env_key} should be set from coordination.json"
            );
        }
        // Case A's remote pair landed — confirmed above.

        // Case B — a pre-existing non-empty value must NOT be clobbered.
        for (_, env_key, _) in &cases {
            std::env::remove_var(env_key);
        }
        std::env::set_var("COXAGENT_REMOTE_STORE_URL", "http://already-set");
        load_coordination(&base);
        assert_eq!(
            std::env::var("COXAGENT_REMOTE_STORE_URL").unwrap(),
            "http://already-set",
            "a pre-existing remote_store_url must not be overwritten"
        );
        // The sibling keys are still populated despite the preset one.
        assert_eq!(std::env::var("COXAGENT_DB_DSN").unwrap(), "postgres://db");
        assert_eq!(
            std::env::var("COXAGENT_REDIS_URL").unwrap(),
            "redis://cache"
        );
        assert_eq!(
            std::env::var("COXAGENT_AUTH_DSN").unwrap(),
            "postgres://auth"
        );
        assert_eq!(std::env::var("COXAGENT_REMOTE_TOKEN").unwrap(), "t0k");

        // Leave no trace behind for parallel tests.
        for (_, env_key, _) in &cases {
            std::env::remove_var(env_key);
        }
    }
}
