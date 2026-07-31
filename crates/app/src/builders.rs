// Part of the composition root split by concern — see lib.rs.
#![allow(clippy::wildcard_imports)]
//! Wiring: everything that turns config into live adapters — stores, auth,
//! engines, storage, MCP access, and the config self-healing.

use super::*;

/// Build the state store for one project, honoring `COXAGENT_DB_DSN`: when set,
/// a shared Postgres store keyed by `id` (multi-tenant); otherwise the local
/// JSON file store rooted at `state_dir`. This is the ports adapter swap — use
/// cases never see which backend they got.
pub(crate) async fn make_store(
    id: &str,
    state_dir: &Path,
) -> Result<Arc<AnyStateStore>, Box<dyn std::error::Error>> {
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
    tracing::info!("coordination config loaded from {}", path.display());
}

pub(crate) fn load_config(state_dir: &Path) -> Config {
    let root = state_dir.parent().unwrap_or(state_dir);
    let path = root.join("coxagent.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                heal_host_port(root, &path, &mut cfg);
                cfg
            }
            Err(e) => {
                tracing::warn!("invalid {}: {e}; using defaults", path.display());
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

/// Self-heal a project left without a deploy port: assign a free `host_port` and
/// persist it, so a project onboarded before per-project ports (or with the field
/// cleared) stops colliding on the shared default port. Picks the lowest port in
/// range that no sibling project claims and that is currently bindable, so two
/// null-port projects on one host land on different ports. Best-effort.
pub(crate) fn heal_host_port(root: &Path, cfg_path: &Path, cfg: &mut Config) {
    if cfg.deploy.host_port.is_some() {
        return;
    }
    // Ports already claimed by sibling projects under the same base dir.
    let mut used: std::collections::HashSet<u16> = std::collections::HashSet::new();
    if let Some(base) = root.parent() {
        if let Ok(entries) = std::fs::read_dir(base) {
            for e in entries.flatten() {
                let sib = e.path().join("coxagent.json");
                if sib == *cfg_path {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&sib) {
                    if let Ok(c) = serde_json::from_str::<Config>(&text) {
                        if let Some(p) = c.deploy.host_port {
                            used.insert(p);
                        }
                    }
                }
            }
        }
    }
    let bindable = |p: u16| std::net::TcpListener::bind(("127.0.0.1", p)).is_ok();
    let Some(port) = (PORT_BASE..PORT_BASE + 500).find(|p| !used.contains(p) && bindable(*p))
    else {
        return;
    };
    cfg.deploy.host_port = Some(port);
    if let Ok(text) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(cfg_path, text);
        tracing::info!("self-healed host port {port} for {}", cfg_path.display());
    }
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
    let config = load_config(state_dir);
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
            match config.git.provider.as_str() {
                "gitlab" => Some(Arc::new(coxagent_infrastructure::GlForge::new(
                    repo, base, wd,
                ))),
                "github" => Some(Arc::new(coxagent_infrastructure::GhForge::new(
                    repo, base, wd,
                ))),
                _ => None,
            }
        } else {
            None
        };
    let forge_for_handle = forge.clone();
    let concurrency = config.workflow.concurrency.max(1);
    let handle = Arc::new(RunnerHandle::new());

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
        .with_janitor(Some(Arc::new(coxagent_infrastructure::OsProcessJanitor)));
        let leader = if let Some(ref f) = forge {
            leader.with_forge(Arc::clone(f))
        } else {
            leader
        };
        let leader = leader.with_notifier(build_notifier(Arc::clone(&store), webhook.clone()));
        let wh = Arc::clone(&handle);
        tokio::spawn(async move { run_forever(wh, leader, sleep).await });
    }

    tracing::info!(
        "[{id}] spawning {} worker runner(s) (total {} runners)",
        concurrency.saturating_sub(1),
        concurrency
    );
    for _ in 1..concurrency {
        let worker = RunCycleUseCase::new(
            Arc::clone(&store),
            engine.clone(),
            config.clone(),
            work_dir.clone(),
            context.clone(),
        )
        .with_meter(meter.clone())
        .with_live_budget(Arc::clone(&live_budget))
        .with_deploy(Arc::new(DockerComposeDeploy::new()))
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
        .with_janitor(Some(Arc::new(coxagent_infrastructure::OsProcessJanitor)));
        let worker = if let Some(ref f) = forge {
            worker.with_forge(Arc::clone(f))
        } else {
            worker
        };
        let worker = worker.with_notifier(build_notifier(Arc::clone(&store), webhook.clone()));
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
