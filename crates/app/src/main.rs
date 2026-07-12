//! CoXAgent composition root — the ONE place dependency injection happens.
//!
//! Parses the CLI, constructs the concrete adapters, and dispatches to the
//! application use cases: report, engine discovery, one-shot BA, the continuous
//! cycle loop (with graceful shutdown), and greenfield onboarding.

mod onboard;
mod shutdown;

use coxagent_application::config::{Config, EngineKind};
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::use_cases::{RecoverUseCase, RunBaUseCase, RunCycleUseCase};
use coxagent_infrastructure::engine::OpencodeEngine;
use coxagent_infrastructure::{discover, JsonStateStore};
use coxagent_presentation::{cli, render_changelog, render_report, Command};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

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
        Command::Onboard { name } => onboard::greenfield(&store, &args.state_dir, &name).await,
        Command::RunBa { work_dir, context } => {
            let config = Config::default();
            let engine = build_opencode(&config)?;
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
        Command::Run {
            work_dir,
            context,
            max_cycles,
        } => run_loop(store, work_dir, context, max_cycles).await,
    }
}

/// The continuous cycle loop with graceful shutdown.
async fn run_loop(
    store: Arc<JsonStateStore>,
    work_dir: PathBuf,
    context: String,
    max_cycles: Option<u64>,
) -> Result<String, Box<dyn std::error::Error>> {
    let config = Config::default();
    let engine = build_opencode(&config)?;
    let sleep = std::time::Duration::from_secs(config.workflow.sleep_seconds);

    // Recovery: release any claims orphaned by a previous crash before looping.
    let recovered = RecoverUseCase::new(Arc::clone(&store)).execute().await?;
    if !recovered.is_empty() {
        tracing::info!("recovered {} orphaned claim(s)", recovered.len());
    }

    let uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context);
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
        if max_cycles.is_some_and(|m| cycle >= m) {
            break;
        }
        shutdown.sleep_or_shutdown(sleep).await;
    }

    tracing::info!("cycle loop stopped after {cycle} cycle(s)");
    Ok(format!("stopped after {cycle} cycle(s)\n"))
}

fn build_opencode(config: &Config) -> Result<Arc<OpencodeEngine>, Box<dyn std::error::Error>> {
    let choice = config.engine.resolve(coxagent_domain::Role::Ba).clone();
    if choice.engine != EngineKind::Opencode {
        return Err(format!(
            "engine {:?} not wired yet (M1 ships opencode)",
            choice.engine
        )
        .into());
    }
    Ok(Arc::new(OpencodeEngine::new(choice.model)))
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
