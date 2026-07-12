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
use coxagent_infrastructure::engine::{AnyEngine, Meter, MeteringEngine};
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
            let (engine, _meter) = build_engine(&config)?;
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

/// Serve the dashboard while hosting the cycle runner in the background. The
/// runner starts paused — the operator resumes/steps it from the dashboard, so
/// hosting the loop never burns engine calls unattended.
async fn serve_with_runner(
    store: Arc<JsonStateStore>,
    state_dir: &Path,
    work_dir: PathBuf,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    use coxagent_application::use_cases::{run_forever, RunCycleUseCase, RunnerHandle};

    let config = load_config(state_dir);
    let (engine, meter) = build_engine(&config)?;
    let sleep = std::time::Duration::from_secs(config.workflow.sleep_seconds);

    // Recover orphaned claims before hosting the loop.
    let recovered = RecoverUseCase::new(Arc::clone(&store)).execute().await?;
    if !recovered.is_empty() {
        tracing::info!("recovered {} orphaned claim(s)", recovered.len());
    }

    let context = std::fs::read_to_string(state_dir.join("project_context.md")).unwrap_or_default();
    let cycle_uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context)
        .with_meter(meter)
        .with_deploy(std::sync::Arc::new(DockerComposeDeploy::new()));
    let handle = Arc::new(RunnerHandle::new());

    let loop_handle = Arc::clone(&handle);
    tokio::spawn(async move { run_forever(loop_handle, cycle_uc, sleep).await });

    let config_path = state_dir
        .parent()
        .unwrap_or(state_dir)
        .join("coxagent.json");
    coxagent_presentation::serve(store, handle, config_path, port).await?;
    Ok(())
}

/// The metered engine plus the spend meter it feeds.
type BuiltEngine = (Arc<MeteringEngine<AnyEngine>>, Meter);

/// Build the metered engine named by the default choice in config, plus the
/// shared spend meter the cycle drains into state.
fn build_engine(config: &Config) -> Result<BuiltEngine, Box<dyn std::error::Error>> {
    let choice = &config.engine.default;
    let inner = AnyEngine::from_choice(choice)?;
    tracing::info!("engine: {} ({})", inner.id(), choice.model);
    let meter: Meter = Arc::new(Mutex::new(Spend::default()));
    Ok((
        Arc::new(MeteringEngine::new(inner, Arc::clone(&meter))),
        meter,
    ))
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
    let (engine, meter) = build_engine(&config)?;
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
