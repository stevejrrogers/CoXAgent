//! CoXAgent composition root — the ONE place dependency injection happens.
//!
//! Parses the CLI, constructs the concrete adapters, and dispatches to the
//! application use cases. Real subcommands land incrementally; M1 wires the BA
//! agent through a real engine plus state reporting and engine discovery.

use coxagent_application::config::{Config, EngineKind};
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::use_cases::RunBaUseCase;
use coxagent_infrastructure::engine::OpencodeEngine;
use coxagent_infrastructure::{discover, JsonStateStore};
use coxagent_presentation::{cli, render_report, Command};
use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::Arc;

#[tokio::main]
async fn main() -> ExitCode {
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

async fn run() -> Result<String, Box<dyn std::error::Error>> {
    let args = cli::parse();
    let store = Arc::new(JsonStateStore::new(&args.state_dir)?);

    match args.command {
        Command::Report => {
            let state = store.load().await?;
            Ok(render_report(&state))
        }
        Command::Discover => Ok(render_discovery()),
        Command::RunBa { work_dir, context } => {
            let config = Config::default();
            let choice = config.engine.resolve(coxagent_domain::Role::Ba).clone();
            // M1 ships the opencode adapter; other engines arrive as adapters.
            if choice.engine != EngineKind::Opencode {
                return Err(format!("engine {:?} not wired yet in M1", choice.engine).into());
            }
            let engine = Arc::new(OpencodeEngine::new(choice.model));
            let uc = RunBaUseCase::new(Arc::clone(&store), engine, config, work_dir, context);
            let created = uc.execute().await?;

            let mut out = format!("BA proposed {} feature(s):\n", created.len());
            for id in &created {
                let _ = writeln!(out, "  + {id}");
            }
            let state = store.load().await?;
            out.push('\n');
            out.push_str(&render_report(&state));
            Ok(out)
        }
    }
}

/// Render detected engines for the `discover` subcommand.
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
