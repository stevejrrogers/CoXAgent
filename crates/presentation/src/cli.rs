//! CLI definition (clap). The presentation layer owns the command *shape*; the
//! app composition root owns the wiring and dispatch.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// `coxagent` — autonomous multi-agent software team.
#[derive(Debug, Parser)]
#[command(name = "coxagent", version, about)]
pub struct Cli {
    /// Directory holding the project state files.
    #[arg(long, global = true, default_value = "./state")]
    pub state_dir: PathBuf,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print a summary of the current project state.
    Report,
    /// List agent engine CLIs detected on this machine's PATH.
    Discover,
    /// Run the BA agent once: propose features and append them to the backlog.
    RunBa {
        /// Working directory handed to the engine (the managed codebase).
        #[arg(long, default_value = ".")]
        work_dir: PathBuf,
        /// One-line product context passed to the BA (until onboarding lands).
        #[arg(long, default_value = "A new software product.")]
        context: String,
    },
}

/// Parse process arguments into a [`Cli`].
#[must_use]
pub fn parse() -> Cli {
    Cli::parse()
}
