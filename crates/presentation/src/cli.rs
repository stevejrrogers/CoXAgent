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
    /// Detect this machine's agent engine CLIs and report them to a hub, so its
    /// dashboard shows them even before any runner cycle starts.
    Probe {
        /// Hub gateway origin without trailing slash (e.g., http://127.0.0.1:4000).
        #[arg(long)]
        hub: String,
        /// Project id whose /store heartbeat records this machine's engines.
        #[arg(long)]
        project: String,
    },
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
    /// Back up the hub's own workspace state — registry, per-project configs
    /// and state, auth, evidence blobs — into one restorable archive under
    /// `<hub-dir>/backups/`. Session tokens are never captured; deploy
    /// secrets only with --include-secrets; the applied policy is stated in
    /// the output.
    Backup {
        /// Hub directory whose state is archived (the registry's directory).
        #[arg(long, default_value = ".")]
        hub_dir: PathBuf,
        /// Archive file to write (default:
        /// <hub-dir>/backups/coxagent-backup-<UTC-timestamp>.hubarchive.json).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Also capture this machine's deploy-secrets root (plaintext inside
        /// the owner-only 0600 archive; restored to the CURRENT machine's root).
        #[arg(long)]
        include_secrets: bool,
    },
    /// Restore a hub workspace archive into <hub-dir> (the one-command move/
    /// recovery side of `backup`). Refuses a non-empty target unless --force,
    /// refuses an archive written by a newer schema, and refuses while a hub
    /// holds a project state lock; --force snapshots the overwritten files to
    /// <hub-dir>/.pre-restore-<ts>/ first.
    Restore {
        /// The .hubarchive.json file to restore from.
        archive: PathBuf,
        /// Hub directory to restore into.
        #[arg(long, default_value = ".")]
        hub_dir: PathBuf,
        /// Overwrite a non-empty target (the overwritten set is snapshotted first).
        #[arg(long)]
        force: bool,
        /// Verify the archive and print the plan without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Read stdin and print a compressed version (rtk-style) — used by the
    /// command shims to shrink noisy tool output before an agent reads it.
    /// Deterministic: no model, no network.
    Compress {
        /// The wrapped command's name (for the summary line).
        #[arg(long)]
        cmd: Option<String>,
        /// Answer whether this command needs byte-exact output instead of
        /// compressing stdin: prints `exact` (or nothing) and reads no input.
        /// The shim uses it to bypass the pipeline entirely for `git`
        /// content-retrieval subcommands, so their stdout, stderr and exit
        /// code stay native.
        #[arg(long)]
        exact_check: bool,
        /// The wrapped command's own arguments, forwarded verbatim so `git`
        /// content-retrieval subcommands (show/diff/log/cat-file/...) can be
        /// detected and passed through byte-exact instead of compressed.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
