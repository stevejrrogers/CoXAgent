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
use coxagent_infrastructure::{discover, DockerComposeDeploy, JsonStateStore};
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

async fn run() -> Result<String, Box<dyn std::error::Error>> {
    let args = cli::parse();
    let store = Arc::new(JsonStateStore::new(&args.state_dir)?);

    match args.command {
        Command::Report => {
            let state = store.load().await?;
            Ok(render_report(&state))
        }
        Command::Discover => Ok(render_discovery()),
        Command::Onboard { name, alias } => {
            onboard::greenfield(&store, &args.state_dir, &name, alias).await
        }
        Command::RunBa { work_dir, context } => {
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
            let changelog = render_changelog(&store.load().await?);
            if let Some(path) = out {
                std::fs::write(&path, &changelog)?;
                Ok(format!("wrote changelog to {}\n", path.display()))
            } else {
                Ok(changelog)
            }
        }
        Command::Check { work_dir } => {
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
            serve_with_runner(store, &args.state_dir, work_dir, port).await?;
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
        } => run_loop(store, &args.state_dir, work_dir, context, max_cycles).await,
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

    let store = Arc::new(JsonStateStore::new(state_dir)?);
    let config = load_config(state_dir);
    let (engine, meter) = build_engine(&config, logs_dir(state_dir))?;
    let sleep = std::time::Duration::from_secs(config.workflow.sleep_seconds);

    let recovered = RecoverUseCase::new(Arc::clone(&store)).execute().await?;
    if !recovered.is_empty() {
        tracing::info!("[{id}] recovered {} orphaned claim(s)", recovered.len());
    }

    let alias = store.load().await.map(|s| s.alias).unwrap_or_default();
    let context = std::fs::read_to_string(state_dir.join("project_context.md")).unwrap_or_default();
    let cycle_uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context)
        .with_meter(meter)
        .with_deploy(Arc::new(DockerComposeDeploy::new()));
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
    })
}

/// Serve a single project (the `serve` command).
async fn serve_with_runner(
    _store: Arc<JsonStateStore>,
    state_dir: &Path,
    work_dir: PathBuf,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let project = build_project("default", state_dir, work_dir).await?;
    coxagent_presentation::serve(vec![project], port).await?;
    Ok(())
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
    let factory: coxagent_presentation::ProjectFactory = Arc::new(move |name, alias| {
        let base = base.clone();
        let registry_path = registry_path.clone();
        Box::pin(async move { onboard_project(&base, &registry_path, &name, alias).await })
    });

    let auth = build_auth(registry)?;
    coxagent_presentation::serve_full(projects, port, Some(factory), auth).await?;
    Ok(())
}

/// Wire RBAC for the hub. An admin can be bootstrapped once via the
/// `COXAGENT_ADMIN_USER` / `COXAGENT_ADMIN_PASSWORD` env vars, which seed a
/// hashed `auth.json` beside the registry. If neither the file nor the env
/// exists, the hub runs open (no login) — handy for local single-user use.
fn build_auth(
    registry: &Path,
) -> Result<Option<Arc<dyn coxagent_application::auth::AuthPort>>, Box<dyn std::error::Error>> {
    use coxagent_infrastructure::FileAuthService;
    let base = registry.parent().unwrap_or_else(|| Path::new("."));
    let auth_path = FileAuthService::default_path(base);

    if let (Ok(user), Ok(pass)) = (
        std::env::var("COXAGENT_ADMIN_USER"),
        std::env::var("COXAGENT_ADMIN_PASSWORD"),
    ) {
        FileAuthService::bootstrap_admin(&auth_path, &user, &pass)?;
    }

    let svc = FileAuthService::open(&auth_path)?;
    if svc.has_users() {
        tracing::info!("RBAC enabled ({} account file)", auth_path.display());
        Ok(Some(Arc::new(svc)))
    } else {
        tracing::info!("no auth configured — running open (set COXAGENT_ADMIN_USER/PASSWORD to enable)");
        Ok(None)
    }
}

/// Scaffold a new project workspace under `base`, seed it, append it to the hub
/// registry, and build a live [`ProjectHandle`]. Used by the dashboard's
/// "new project" flow.
async fn onboard_project(
    base: &Path,
    registry_path: &Path,
    name: &str,
    alias: Option<String>,
) -> Result<coxagent_presentation::ProjectHandle, String> {
    let derived = alias.clone().unwrap_or_else(|| {
        coxagent_application::state::derive_alias(name)
    });
    let id = unique_id(base, &derived.to_lowercase());
    let proj_dir = base.join(&id);
    let state_dir = proj_dir.join("state");
    let work_dir = proj_dir.join("codebase");
    std::fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let store = Arc::new(JsonStateStore::new(&state_dir).map_err(|e| e.to_string())?);
    onboard::greenfield(&store, &state_dir, name, alias)
        .await
        .map_err(|e| e.to_string())?;

    append_registry(registry_path, &id, &proj_dir).map_err(|e| e.to_string())?;

    build_project(&id, &state_dir, work_dir)
        .await
        .map_err(|e| e.to_string())
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
    store: Arc<JsonStateStore>,
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

    let uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context)
        .with_meter(meter)
        .with_deploy(std::sync::Arc::new(DockerComposeDeploy::new()));
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
