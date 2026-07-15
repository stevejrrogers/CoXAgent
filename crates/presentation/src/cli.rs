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
    /// Scaffold a new project workspace (greenfield), or adopt an existing
    /// codebase with `--existing <path>` (brownfield).
    Onboard {
        /// Human-readable project name.
        #[arg(long)]
        name: String,
        /// Short ticket-id alias (e.g. CXC). Auto-derived from the name if omitted.
        #[arg(long)]
        alias: Option<String>,
        /// Adopt an existing codebase at this path instead of scaffolding a new
        /// one: initialises git if needed and seeds follow-up work.
        #[arg(long)]
        existing: Option<PathBuf>,
    },
    /// Render the changelog from deploy history (writes to a file if given).
    Changelog {
        /// Optional path to write the changelog to (prints to stdout otherwise).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run architecture-conformance governance and file bugs for any drift.
    Check {
        /// Managed codebase directory to scan.
        #[arg(long, default_value = ".")]
        work_dir: PathBuf,
    },
    /// Serve the web dashboard and host the controllable cycle runner.
    Serve {
        /// Port to listen on.
        #[arg(long, default_value_t = 4000)]
        port: u16,
        /// Managed codebase directory the agents work in.
        #[arg(long, default_value = ".")]
        work_dir: PathBuf,
    },
    /// Serve many projects from a hub registry (multi-project mode).
    Hub {
        /// Path to the hub registry JSON: an array of `{ "id", "path" }`.
        #[arg(long)]
        registry: PathBuf,
        /// Port to listen on.
        #[arg(long, default_value_t = 4000)]
        port: u16,
    },
    /// Read stdin and print a compressed version (rtk-style) — used by the
    /// command shims to shrink noisy tool output before an agent reads it.
    /// Deterministic: no model, no network.
    Compress {
        /// The wrapped command's name (for the summary line).
        #[arg(long)]
        cmd: Option<String>,
    },
    /// Query the code knowledge graph (for agents + humans): symbol search,
    /// impact/references, callers, and file dependencies.
    Codegraph {
        #[command(subcommand)]
        query: CodegraphQuery,
        /// Codebase directory (defaults to the current directory). Accepted
        /// before or after the subcommand.
        #[arg(long, default_value = ".", global = true)]
        work_dir: PathBuf,
    },
}

/// Code-graph query subcommands.
#[derive(Subcommand, Debug)]
pub enum CodegraphQuery {
    /// (Re)build the index for the working tree.
    Build,
    /// Find symbols whose name contains QUERY.
    Search { query: String },
    /// Every usage of NAME across the tree (comment/string-free), with callers.
    Impact { name: String },
    /// Functions that call NAME (what breaks if you change it).
    Callers { name: String },
    /// Files that import FILE's module.
    Deps { file: String },
    /// The compact repo map agents read to orient.
    Map,
}

/// Parse process arguments into a [`Cli`].
#[must_use]
pub fn parse() -> Cli {
    Cli::parse()
}
