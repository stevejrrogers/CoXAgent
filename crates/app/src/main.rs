//! CoXAgent composition root — the ONE place dependency injection happens.
//!
//! Parses the CLI, constructs the concrete adapters, and dispatches to the
//! application use cases: report, engine discovery, one-shot BA, the continuous
//! cycle loop (with graceful shutdown), and greenfield onboarding.

mod onboard;
mod shutdown;

use coxagent_application::config::Config;
use coxagent_application::ports::outbound::{AgentEnginePort, StateStorePort};
use coxagent_application::use_cases::{RecoverUseCase, RunBaUseCase, RunCycleUseCase};
use coxagent_application::Spend;
use coxagent_infrastructure::engine::{AnyEngine, Meter, MeteringEngine, TranscriptEngine};
use coxagent_infrastructure::{
    discover, AnyStateStore, DockerComposeDeploy, JsonStateStore, SqlStateStore, WebhookNotifier,
};
use coxagent_presentation::{cli, render_changelog, render_report, Command};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("coxagent: {err}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}

/// Build the state store for one project, honoring `COXAGENT_DB_DSN`: when set,
/// a shared Postgres store keyed by `id` (multi-tenant); otherwise the local
/// JSON file store rooted at `state_dir`. This is the ports adapter swap — use
/// cases never see which backend they got.
async fn make_store(
    id: &str,
    state_dir: &Path,
) -> Result<Arc<AnyStateStore>, Box<dyn std::error::Error>> {
    match std::env::var("COXAGENT_DB_DSN") {
        Ok(dsn) if !dsn.is_empty() => {
            tracing::info!("[{id}] state store: Postgres");
            Ok(Arc::new(AnyStateStore::Sql(
                SqlStateStore::connect(&dsn, id).await?,
            )))
        }
        _ => Ok(Arc::new(AnyStateStore::Json(JsonStateStore::new(
            state_dir,
        )?))),
    }
}

async fn run() -> Result<String, Box<dyn std::error::Error>> {
    let args = cli::parse();
    // The single-project store is built lazily: `serve`/`hub`/`discover` don't
    // use it, so we must not create it eagerly — the default `./state` would
    // resolve against a read-only cwd (e.g. a GUI-launched app runs in `/`).
    let store = || make_store("default", &args.state_dir);

    match args.command {
        Command::Report => {
            let state = store().await?.load().await?;
            Ok(render_report(&state))
        }
        Command::Discover => Ok(render_discovery()),
        Command::Onboard {
            name,
            alias,
            existing,
        } => {
            let store = store().await?;
            match existing {
                Some(codebase) => {
                    onboard::brownfield(&store, &args.state_dir, &name, alias, &codebase).await
                }
                None => onboard::greenfield(&store, &args.state_dir, &name, alias).await,
            }
        }
        Command::RunBa { work_dir, context } => {
            let store = store().await?;
            let config = load_config(&args.state_dir);
            let (engine, _meter) = build_engine(&config, logs_dir(&args.state_dir))?;
            let uc = RunBaUseCase::new(Arc::clone(&store), engine, config, work_dir, context);
            let created = uc.execute().await?;
            let mut out = format!("BA proposed {} feature(s):\n", created.len());
            for id in &created {
                let _ = writeln!(out, "  + {id}");
            }
            out.push('\n');
            out.push_str(&render_report(&store.load().await?));
            Ok(out)
        }
        Command::Changelog { out } => {
            let changelog = render_changelog(&store().await?.load().await?);
            if let Some(path) = out {
                std::fs::write(&path, &changelog)?;
                Ok(format!("wrote changelog to {}\n", path.display()))
            } else {
                Ok(changelog)
            }
        }
        Command::Check { work_dir } => {
            let store = store().await?;
            let config = load_config(&args.state_dir);
            let uc = coxagent_application::use_cases::RunConformanceUseCase::new(
                Arc::clone(&store),
                work_dir,
                config.architecture,
            );
            let filed = uc.execute().await?;
            if filed.is_empty() {
                Ok("architecture conformance: OK (no drift)\n".to_owned())
            } else {
                let mut out = format!("architecture drift — filed {} bug(s):\n", filed.len());
                for id in &filed {
                    let _ = writeln!(out, "  {id}");
                }
                Ok(out)
            }
        }
        Command::Serve { port, work_dir } => {
            serve_with_runner(&args.state_dir, work_dir, port).await?;
            Ok(String::new())
        }
        Command::Hub { registry, port } => {
            run_hub(&registry, port).await?;
            Ok(String::new())
        }
        Command::Run {
            work_dir,
            context,
            max_cycles,
        } => {
            run_loop(
                store().await?,
                &args.state_dir,
                work_dir,
                context,
                max_cycles,
            )
            .await
        }
    }
}

/// Load `coxagent.json` from the workspace root (parent of the state dir), or
/// fall back to defaults. Config lives beside the state, written by `onboard`.
fn load_config(state_dir: &Path) -> Config {
    let root = state_dir.parent().unwrap_or(state_dir);
    let path = root.join("coxagent.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("invalid {}: {e}; using defaults", path.display());
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

/// Build one project: store, engine stack, runner (spawned, paused), returned as
/// a `ProjectHandle` the hub server can host alongside others.
async fn build_project(
    id: &str,
    state_dir: &Path,
    work_dir: PathBuf,
) -> Result<coxagent_presentation::ProjectHandle, Box<dyn std::error::Error>> {
    use coxagent_application::use_cases::{run_forever, RunCycleUseCase, RunnerHandle};

    let store = make_store(id, state_dir).await?;
    let config = load_config(state_dir);
    let (engine, meter) = build_engine(&config, logs_dir(state_dir))?;
    let sleep = std::time::Duration::from_secs(config.workflow.sleep_seconds);

    let recovered = RecoverUseCase::new(Arc::clone(&store)).execute().await?;
    if !recovered.is_empty() {
        tracing::info!("[{id}] recovered {} orphaned claim(s)", recovered.len());
    }

    let alias = store.load().await.map(|s| s.alias).unwrap_or_default();
    let context = std::fs::read_to_string(state_dir.join("project_context.md")).unwrap_or_default();
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
    let mut cycle_uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context)
        .with_meter(meter)
        .with_live_budget(Arc::clone(&live_budget))
        .with_deploy(Arc::new(DockerComposeDeploy::new()));
    if let Some(url) = webhook.filter(|u| !u.is_empty()) {
        cycle_uc = cycle_uc.with_notifier(Arc::new(WebhookNotifier::new(url)));
    }
    let handle = Arc::new(RunnerHandle::new());
    let loop_handle = Arc::clone(&handle);
    tokio::spawn(async move { run_forever(loop_handle, cycle_uc, sleep).await });

    let config_path = state_dir
        .parent()
        .unwrap_or(state_dir)
        .join("coxagent.json");
    Ok(coxagent_presentation::ProjectHandle {
        id: id.to_owned(),
        name: if alias.is_empty() {
            id.to_owned()
        } else {
            format!("{alias} project")
        },
        alias,
        store,
        runner: handle,
        config_path,
        engine: engine_for_handle,
        work_dir: work_dir_for_handle,
        budget: live_budget,
        context_path: state_dir.join("project_context.md"),
    })
}

/// Serve a single project (the `serve` command).
async fn serve_with_runner(
    state_dir: &Path,
    work_dir: PathBuf,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let project = build_project("default", state_dir, work_dir).await?;
    let audit = build_audit().await;
    // Single-project serve honors the same RBAC env vars as the hub.
    let auth = build_auth(state_dir.parent().unwrap_or(state_dir)).await?;
    let extras = coxagent_presentation::HubExtras {
        auth,
        engines: detected_engines(),
        ..Default::default()
    };
    coxagent_presentation::serve_full(vec![project], port, audit, extras).await?;
    Ok(())
}

/// Engine CLIs found on PATH, as `(name, path)` for the dashboard.
fn detected_engines() -> Vec<(String, String)> {
    discover()
        .into_iter()
        .map(|d| (d.kind.as_binary().to_owned(), d.path.display().to_string()))
        .collect()
}

/// Build the security-audit sink: Postgres when `COXAGENT_DB_DSN` is set (the
/// trail then survives restarts and is shared across the hub), else in memory.
async fn build_audit() -> Arc<dyn coxagent_application::ports::outbound::AuditPort> {
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

/// Serve many projects from a hub registry file (the `hub` command). The
/// registry is a JSON array of `{ "id", "path" }` where `path` is a workspace
/// dir containing `state/` and `codebase/`.
async fn run_hub(registry: &Path, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(serde::Deserialize)]
    struct Entry {
        id: String,
        path: PathBuf,
    }
    let text = std::fs::read_to_string(registry)
        .map_err(|e| format!("cannot read hub registry {}: {e}", registry.display()))?;
    let entries: Vec<Entry> = serde_json::from_str(&text)?;
    if entries.is_empty() {
        return Err("hub registry is empty".into());
    }

    let mut projects = Vec::new();
    for e in entries {
        let state_dir = e.path.join("state");
        let work_dir = e.path.join("codebase");
        match build_project(&e.id, &state_dir, work_dir).await {
            Ok(p) => {
                tracing::info!("hub: registered project '{}'", p.id);
                projects.push(p);
            }
            Err(err) => tracing::warn!("hub: skipping '{}': {err}", e.id),
        }
    }
    if projects.is_empty() {
        return Err("no projects could be registered".into());
    }

    // Factory: onboard a brand-new project from the dashboard. New workspaces
    // land under the registry's directory and are appended to the registry file
    // so they survive a restart.
    let base = registry
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let registry_path = registry.to_path_buf();
    let factory: coxagent_presentation::ProjectFactory = Arc::new({
        let base = base.clone();
        let registry_path = registry_path.clone();
        move |req| {
            let base = base.clone();
            let registry_path = registry_path.clone();
            Box::pin(async move { onboard_project(&base, &registry_path, req).await })
        }
    });
    let remover: coxagent_presentation::ProjectRemover = Arc::new({
        let registry_path = registry_path.clone();
        move |id| {
            let registry_path = registry_path.clone();
            Box::pin(async move { remove_from_registry(&registry_path, &id) })
        }
    });

    // A hub-level engine for cross-project drafting (e.g. project goals), built
    // from the first project's config; its work dir is the registry directory.
    let analyzer = build_engine(&Config::default(), logs_dir(&base))
        .ok()
        .map(|(e, _)| {
            let engine: Arc<dyn coxagent_application::ports::outbound::AgentEnginePort> = e;
            (engine, base.clone())
        });

    let auth = build_auth(registry.parent().unwrap_or_else(|| Path::new("."))).await?;
    let audit = build_audit().await;
    let extras = coxagent_presentation::HubExtras {
        factory: Some(factory),
        remover: Some(remover),
        auth,
        engines: detected_engines(),
        analyzer,
    };
    coxagent_presentation::serve_full(projects, port, audit, extras).await?;
    Ok(())
}

/// Remove a project entry (by id) from the hub registry file.
fn remove_from_registry(registry_path: &Path, id: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(registry_path).map_err(|e| e.to_string())?;
    let mut arr: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    arr.retain(|e| e.get("id").and_then(|v| v.as_str()) != Some(id));
    std::fs::write(
        registry_path,
        serde_json::to_string_pretty(&arr).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

/// Wire RBAC. An admin is (re-)provisioned from the `COXAGENT_ADMIN_USER` /
/// `COXAGENT_ADMIN_PASSWORD` env vars into `auth.json` under `base` — the env is
/// authoritative, so setting it always makes that password work even if a stale
/// file exists. With no file and no env, the server runs open (no login).
async fn build_auth(
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

    // Server mode: when a database is configured, accounts / membership / tokens
    // live in Postgres (shared across instances), not a local file.
    if let Ok(dsn) = std::env::var("COXAGENT_DB_DSN") {
        let svc = SqlAuthService::connect(&dsn).await?;
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

/// Scaffold a new project workspace under `base`, seed it, append it to the hub
/// registry, and build a live [`ProjectHandle`]. Used by the dashboard's
/// "new project" flow.
async fn onboard_project(
    base: &Path,
    registry_path: &Path,
    req: coxagent_presentation::NewProjectReq,
) -> Result<coxagent_presentation::ProjectHandle, String> {
    let name = req.name.trim();
    let derived = req
        .alias
        .clone()
        .unwrap_or_else(|| coxagent_application::state::derive_alias(name));
    let id = unique_id(base, &derived.to_lowercase());
    let proj_dir = base.join(&id);
    let state_dir = proj_dir.join("state");
    std::fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;

    let store = make_store(&id, &state_dir)
        .await
        .map_err(|e| e.to_string())?;

    // Brownfield import: adopt the given codebase in place. Greenfield: scaffold
    // a fresh `codebase/` under the workspace.
    let work_dir = if let Some(path) = &req.existing {
        onboard::brownfield(&store, &state_dir, name, req.alias.clone(), path)
            .await
            .map_err(|e| e.to_string())?;
        path.clone()
    } else {
        let wd = proj_dir.join("codebase");
        std::fs::create_dir_all(&wd).map_err(|e| e.to_string())?;
        onboard::greenfield(&store, &state_dir, name, req.alias.clone())
            .await
            .map_err(|e| e.to_string())?;
        wd
    };

    // Seed the confirmed project brief (from AI-assisted drafting), if any.
    if let Some(goal) = req.goal.as_ref().filter(|g| !g.trim().is_empty()) {
        let _ = std::fs::write(state_dir.join("project_context.md"), goal);
    }

    // Assign a unique host port so this project's `docker compose` deploy does
    // not clash with the others on this host.
    assign_host_port(base, registry_path, &proj_dir).map_err(|e| e.to_string())?;

    append_registry(registry_path, &id, &proj_dir).map_err(|e| e.to_string())?;

    build_project(&id, &state_dir, work_dir)
        .await
        .map_err(|e| e.to_string())
}

/// First host port for auto-allocation.
const PORT_BASE: u16 = 8100;

/// Write a free `deploy.host_port` into the new project's `coxagent.json`,
/// picking the lowest port from [`PORT_BASE`] not already used by a registered
/// project. Best-effort — a failure just leaves the port unset.
fn assign_host_port(
    base: &Path,
    registry_path: &Path,
    proj_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use coxagent_application::Config;
    // Collect ports already taken by registered projects.
    let mut used: std::collections::HashSet<u16> = std::collections::HashSet::new();
    if let Ok(text) = std::fs::read_to_string(registry_path) {
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&text) {
            for e in arr {
                if let Some(p) = e.get("path").and_then(|p| p.as_str()) {
                    let cfg = Path::new(p).join("coxagent.json");
                    if let Ok(c) = std::fs::read_to_string(&cfg) {
                        if let Ok(c) = serde_json::from_str::<Config>(&c) {
                            if let Some(port) = c.deploy.host_port {
                                used.insert(port);
                            }
                        }
                    }
                }
            }
        }
    }
    let _ = base; // reserved for future host-wide allocation policy
    let port = (PORT_BASE..PORT_BASE + 500)
        .find(|p| !used.contains(p))
        .unwrap_or(PORT_BASE);

    let cfg_path = proj_dir.join("coxagent.json");
    let mut cfg: Config = std::fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    cfg.deploy.host_port = Some(port);
    std::fs::write(&cfg_path, serde_json::to_string_pretty(&cfg)?)?;
    tracing::info!("assigned host port {port} to new project");
    Ok(())
}

/// Pick an id not already taken by a workspace directory under `base`.
fn unique_id(base: &Path, seed: &str) -> String {
    let seed = if seed.is_empty() { "project" } else { seed };
    if !base.join(seed).exists() {
        return seed.to_owned();
    }
    (2..10_000)
        .map(|n| format!("{seed}-{n}"))
        .find(|c| !base.join(c).exists())
        .unwrap_or_else(|| format!("{seed}-x"))
}

/// Append `{ id, path }` to the hub registry JSON array (best-effort persistence).
fn append_registry(registry_path: &Path, id: &str, path: &Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(registry_path).unwrap_or_else(|_| "[]".to_owned());
    let mut arr: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    arr.push(serde_json::json!({ "id": id, "path": path }));
    std::fs::write(registry_path, serde_json::to_string_pretty(&arr)?)
}

/// The metered + transcript-logging engine plus the spend meter it feeds.
type BuiltEngine = (Arc<MeteringEngine<TranscriptEngine<AnyEngine>>>, Meter);

/// Build the engine stack (transcript logging + metering) named by config,
/// plus the shared spend meter the cycle drains into state.
fn build_engine(
    config: &Config,
    logs_dir: PathBuf,
) -> Result<BuiltEngine, Box<dyn std::error::Error>> {
    let choice = &config.engine.default;
    let inner = AnyEngine::from_choice(choice)?;
    tracing::info!("engine: {} ({})", inner.id(), choice.model);
    let logged = TranscriptEngine::new(inner, logs_dir);
    let meter: Meter = Arc::new(Mutex::new(Spend::default()));
    Ok((
        Arc::new(MeteringEngine::new(logged, Arc::clone(&meter))),
        meter,
    ))
}

/// Transcripts live under `<workspace>/logs/transcripts`.
fn logs_dir(state_dir: &Path) -> PathBuf {
    state_dir
        .parent()
        .unwrap_or(state_dir)
        .join("logs")
        .join("transcripts")
}

/// The continuous cycle loop with graceful shutdown.
async fn run_loop(
    store: Arc<AnyStateStore>,
    state_dir: &Path,
    work_dir: PathBuf,
    context: String,
    max_cycles: Option<u64>,
) -> Result<String, Box<dyn std::error::Error>> {
    let config = load_config(state_dir);
    let (engine, meter) = build_engine(&config, logs_dir(state_dir))?;
    let sleep = std::time::Duration::from_secs(config.workflow.sleep_seconds);

    // Recovery: release any claims orphaned by a previous crash before looping.
    let recovered = RecoverUseCase::new(Arc::clone(&store)).execute().await?;
    if !recovered.is_empty() {
        tracing::info!("recovered {} orphaned claim(s)", recovered.len());
    }

    let webhook = config.workflow.webhook_url.clone();
    let mut uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context)
        .with_meter(meter)
        .with_deploy(std::sync::Arc::new(DockerComposeDeploy::new()));
    if let Some(url) = webhook.filter(|u| !u.is_empty()) {
        uc = uc.with_notifier(std::sync::Arc::new(WebhookNotifier::new(url)));
    }
    let shutdown = shutdown::Shutdown::listen();
    tracing::info!("cycle loop started");

    let mut cycle = 0u64;
    while !shutdown.is_triggered() {
        cycle += 1;
        let report = uc.run_cycle(cycle).await;
        tracing::info!("{}", report.summary());
        for e in &report.errors {
            tracing::warn!("{e}");
        }
        if report.over_budget {
            tracing::warn!("budget cap reached — stopping loop");
            break;
        }
        if max_cycles.is_some_and(|m| cycle >= m) {
            break;
        }
        shutdown.sleep_or_shutdown(sleep).await;
    }

    tracing::info!("cycle loop stopped after {cycle} cycle(s)");
    Ok(format!("stopped after {cycle} cycle(s)\n"))
}

fn render_discovery() -> String {
    let found = discover();
    if found.is_empty() {
        return "No agent engines detected on PATH.\n".to_owned();
    }
    let mut out = String::from("Detected engines:\n");
    for d in found {
        let _ = writeln!(out, "  {:<10} {}", d.kind.as_binary(), d.path.display());
    }
    out
}
