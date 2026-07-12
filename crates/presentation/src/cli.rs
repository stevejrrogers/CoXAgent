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
    /// Run the continuous cycle loop (BA → DEV-BUG → DEV-FEATURE → TEST).
    Run {
        /// Managed codebase directory.
        #[arg(long, default_value = ".")]
        work_dir: PathBuf,
        /// Product context passed to the BA agent.
        #[arg(long, default_value = "A new software product.")]
        context: String,
        /// Stop after this many cycles (default: run until interrupted).
        #[arg(long)]
        max_cycles: Option<u64>,
    },
    /// Scaffold a new project workspace (greenfield onboarding).
    Onboard {
        /// Human-readable project name.
        #[arg(long)]
        name: String,
    },
    /// Render the changelog from deploy history (writes to a file if given).
    Changelog {
        /// Optional path to write the changelog to (prints to stdout otherwise).
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

/// Parse process arguments into a [`Cli`].
#[must_use]
pub fn parse() -> Cli {
    Cli::parse()
}
