//! CoXAgent composition root — the ONE place dependency injection happens.
//!
//! Parses the CLI, constructs the concrete adapters, and dispatches to the
//! application use cases: report, engine discovery, one-shot BA, the continuous
//! cycle loop (with graceful shutdown), and greenfield onboarding.

mod onboard;
mod shutdown;

use coxagent_application::config::Config;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::use_cases::{RecoverUseCase, RunBaUseCase, RunCycleUseCase};
use coxagent_application::Spend;
use coxagent_infrastructure::engine::{
    AnyEngine, FailoverEngine, Meter, MeteringEngine, RoutingEngine, TranscriptEngine,
};
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

#[allow(clippy::too_many_lines)] // a flat CLI-command dispatch; splitting hurts readability
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
            // Pick up the shared coordination backend (Postgres/Redis) from a
            // persistent file next to the registry, so the desktop app is
            // distributed even when launched from Finder (no env).
            load_coordination(registry.parent().unwrap_or_else(|| Path::new(".")));
            run_hub(&registry, port).await?;
            Ok(String::new())
        }
        Command::Run {
            work_dir,
            context,
            max_cycles,
        } => {
            load_coordination(
                args.state_dir
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or(&args.state_dir),
            );
            // The project id is the workspace dir name (e.g. `cxc`), NOT a fixed
            // "default" — so a headless worker shares the SAME Postgres project as
            // the hub and other operators (distributed coordination).
            let pid = args
                .state_dir
                .parent()
                .and_then(Path::file_name)
                .map_or_else(
                    || "default".to_owned(),
                    |n| n.to_string_lossy().into_owned(),
                );
            run_loop(
                make_store(&pid, &args.state_dir).await?,
                &args.state_dir,
                work_dir,
                context,
                max_cycles,
            )
            .await
        }
        Command::Codegraph { query, work_dir } => codegraph_query(&work_dir, &query),
        Command::Compress { cmd: _ } => {
            use std::io::Read as _;
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).ok();
            let out = coxagent_application::tokens::proxy_compress(&input);
            // Record the saving so the dashboard can show how effective the
            // token-saver is (appends "before after" to the shim dir's log).
            record_compression(input.len(), out.len());
            Ok(out)
        }
    }
}

/// Write the command-output shims (rtk-style) into a temp dir and return it.
/// Each shim runs the real command and pipes its output through
/// `coxagent compress` (only for non-tty, large output — small/exact output is
/// untouched). Applied to agent subprocesses only, so the hub's own tooling is
/// never affected.
/// Verbose, output-heavy commands worth compressing. `git` is included but the
/// small-output passthrough keeps porcelain (rev-parse/status) exact.
const SHIM_CMDS: &[&str] = &[
    "cargo", "npm", "pnpm", "yarn", "pip", "pip3", "pytest", "go", "gradle", "mvn", "make",
    "docker", "git", "node", "python", "python3", "tsc", "jest", "vitest",
];

fn setup_command_shims() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = std::env::temp_dir().join("coxagent-shims");
    std::fs::create_dir_all(&dir).ok()?;
    let dir_disp = dir.display().to_string();
    let exe_disp = exe.display().to_string();
    for cmd in SHIM_CMDS {
        let script = format!(
            "#!/usr/bin/env bash\n\
             cmd=\"{cmd}\"\n\
             real=\"\"\n\
             _IFS=\"$IFS\"; IFS=:\n\
             for d in $PATH; do\n\
             \x20 [ \"$d\" = \"{dir_disp}\" ] && continue\n\
             \x20 if [ -x \"$d/$cmd\" ]; then real=\"$d/$cmd\"; break; fi\n\
             done\n\
             IFS=\"$_IFS\"\n\
             [ -z \"$real\" ] && {{ echo \"cox-shim: $cmd not found\" >&2; exit 127; }}\n\
             if [ \"${{COX_COMPRESS:-1}}\" = \"1\" ] && [ ! -t 1 ]; then\n\
             \x20 set -o pipefail\n\
             \x20 \"$real\" \"$@\" 2>&1 | \"{exe_disp}\" compress --cmd \"$cmd\"\n\
             \x20 exit \"${{PIPESTATUS[0]:-0}}\"\n\
             fi\n\
             exec \"$real\" \"$@\"\n"
        );
        let p = dir.join(cmd);
        if std::fs::write(&p, script).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    Some(dir)
}

/// Generate the shims and advertise them to agent subprocesses via
/// `COXAGENT_SHIM_DIR` (the engine prepends it to the child's PATH). Opt-out
/// with `COX_COMPRESS=0`. Best-effort — never fatal.
fn enable_command_shims() {
    if std::env::var("COX_COMPRESS").as_deref() == Ok("0") {
        return;
    }
    if let Some(dir) = setup_command_shims() {
        std::env::set_var("COXAGENT_SHIM_DIR", dir);
    }
}

/// Answer a code-graph query for agents (and humans) — structured, token-cheap
/// output instead of grepping the tree by hand.
fn codegraph_query(
    work_dir: &Path,
    query: &coxagent_presentation::CodegraphQuery,
) -> Result<String, Box<dyn std::error::Error>> {
    use coxagent_application::codegraph::{references, CodeGraph};
    use coxagent_presentation::CodegraphQuery as Q;
    use std::fmt::Write as _;

    // Build fresh for `build`; otherwise use the persisted index (build if absent).
    let graph = || CodeGraph::load(work_dir).unwrap_or_else(|| CodeGraph::index(work_dir));
    let mut out = String::new();
    match query {
        Q::Build => {
            let g = CodeGraph::index(work_dir);
            g.save(work_dir)?;
            let _ = std::fs::write(
                work_dir.join(".coxagent").join("REPO_MAP.md"),
                g.repo_map(40_000),
            );
            let _ = writeln!(
                out,
                "indexed {} files, {} symbols, {} calls",
                g.files.len(),
                g.symbols.len(),
                g.calls.len()
            );
        }
        Q::Search { query: q } => {
            let g = graph();
            for s in g.relevance_search(q, 50) {
                let scope = s
                    .scope
                    .as_deref()
                    .map_or(String::new(), |sc| format!("{sc}::"));
                let _ = writeln!(out, "{} {scope}{}  {}:{}", s.kind, s.name, s.file, s.line);
            }
        }
        Q::Impact { name } => {
            let refs = references(work_dir, name, 200);
            let (defs, uses): (Vec<_>, Vec<_>) = refs.iter().partition(|r| r.is_def);
            let _ = writeln!(
                out,
                "{} definition(s), {} usage(s):",
                defs.len(),
                uses.len()
            );
            for r in &refs {
                let tag = if r.is_def { "def" } else { "use" };
                let _ = writeln!(out, "  {tag} {}:{}  {}", r.file, r.line, r.text);
            }
        }
        Q::Callers { name } => {
            let g = graph();
            let callers = g.callers(name);
            if callers.is_empty() {
                let _ = writeln!(out, "no callers found for `{name}`");
            }
            for (who, file, line) in callers {
                let _ = writeln!(out, "{who}  {file}:{line}");
            }
        }
        Q::Deps { file } => {
            let g = graph();
            for f in g.dependents(file) {
                let _ = writeln!(out, "{f}");
            }
        }
        Q::Map => {
            out.push_str(&graph().repo_map(40_000));
        }
    }
    Ok(out.trim_end().to_owned())
}

/// Load `coxagent.json` from the workspace root (parent of the state dir), or
/// fall back to defaults. Config lives beside the state, written by `onboard`.
/// Load the shared coordination backend (Postgres state DSN + Redis URL) from
/// `<base>/coordination.json` into the environment, unless already set. Lets the
/// Finder-launched app join the distributed backend without env plumbing.
fn load_coordination(base: &Path) {
    if std::env::var("COXAGENT_DB_DSN").is_ok_and(|v| !v.is_empty()) {
        return; // an explicit env always wins
    }
    let path = base.join("coordination.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
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

fn load_config(state_dir: &Path) -> Config {
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
fn heal_host_port(root: &Path, cfg_path: &Path, cfg: &mut Config) {
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

    let loaded = store.load().await.ok();
    let alias = loaded.as_ref().map(|s| s.alias.clone()).unwrap_or_default();
    let custom_name = loaded.as_ref().and_then(|s| s.display_name.clone());
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
    let mut cycle_uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context)
        .with_meter(meter)
        .with_live_budget(Arc::clone(&live_budget))
        .with_deploy(Arc::new(DockerComposeDeploy::new()))
        .with_git(Arc::new(coxagent_infrastructure::SystemGit::new()));
    if let Some(f) = forge {
        cycle_uc = cycle_uc.with_forge(f);
    }
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
        deploy: Some(Arc::new(DockerComposeDeploy::new())),
    })
}

/// Serve a single project (the `serve` command).
async fn serve_with_runner(
    state_dir: &Path,
    work_dir: PathBuf,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_command_shims();
    let project = build_project("default", state_dir, work_dir).await?;
    let audit = build_audit().await;
    // Single-project serve honors the same RBAC env vars as the hub.
    let auth = build_auth(state_dir.parent().unwrap_or(state_dir)).await?;
    let extras = coxagent_presentation::HubExtras {
        auth,
        engines: detected_engines(),
        tooling: detected_tooling(),
        // System chat + its media live alongside the project state.
        hub_dir: Some(state_dir.parent().unwrap_or(state_dir).to_path_buf()),
        storage: build_storage().await,
        doc_store: build_doc_store().await,
        ..Default::default()
    };
    coxagent_presentation::serve_full(vec![project], port, audit, extras).await?;
    Ok(())
}

/// Documentation store: MongoDB when `COXAGENT_MONGO_URL` is set, else `None`
/// (docs fall back to per-project `state.json`).
async fn build_doc_store(
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
async fn build_storage(
) -> Option<std::sync::Arc<dyn coxagent_application::ports::outbound::StoragePort>> {
    let s3 = coxagent_infrastructure::S3Storage::from_env()?;
    match s3.ensure_bucket().await {
        Ok(()) => tracing::info!("blob storage: S3/MinIO"),
        Err(e) => tracing::warn!("S3 bucket check failed ({e}); uploads may fail"),
    }
    Some(std::sync::Arc::new(s3))
}

/// Engine CLIs found on PATH, as `(name, path)` for the dashboard.
fn detected_engines() -> Vec<(String, String)> {
    discover()
        .into_iter()
        .map(|d| (d.kind.as_binary().to_owned(), d.path.display().to_string()))
        .collect()
}

/// Developer tooling status (git/gh/glab/docker) serialized for the dashboard.
fn detected_tooling() -> serde_json::Value {
    serde_json::to_value(coxagent_infrastructure::discover_tooling()).unwrap_or_default()
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
    enable_command_shims();
    let text = std::fs::read_to_string(registry)
        .map_err(|e| format!("cannot read hub registry {}: {e}", registry.display()))?;
    let entries: Vec<Entry> = serde_json::from_str(&text)?;
    if entries.is_empty() {
        // A fresh install starts with no projects — serve anyway so the user can
        // create the first one from the dashboard ("New project").
        tracing::info!("hub: empty registry — serving with no projects yet");
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
        tooling: detected_tooling(),
        analyzer,
        // System-wide chat lives at the hub root (next to the registry).
        hub_dir: Some(base.clone()),
        storage: build_storage().await,
        doc_store: build_doc_store().await,
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

/// The metered + transcript-logging + per-role-routing engine plus the spend
/// meter it feeds.
type BuiltEngine = (
    Arc<MeteringEngine<TranscriptEngine<RoutingEngine<FailoverEngine<AnyEngine>>>>>,
    Meter,
);

/// Build one failover chain: the primary choice first, then any configured
/// fallbacks, so a quota wall on one CLI rolls over to the next.
fn build_failover(
    choice: &coxagent_application::config::EngineChoice,
    fallbacks: &[coxagent_application::config::EngineChoice],
) -> Result<FailoverEngine<AnyEngine>, Box<dyn std::error::Error>> {
    let mut engines = vec![AnyEngine::from_choice(choice)?];
    for fb in fallbacks {
        match AnyEngine::from_choice(fb) {
            Ok(e) => engines.push(e),
            Err(e) => tracing::warn!("skipping fallback engine {:?}: {e}", fb.engine),
        }
    }
    Ok(FailoverEngine::new(engines))
}

/// Build the engine stack (per-role routing over failover chains + transcript
/// logging + metering) named by config, plus the shared spend meter the cycle
/// drains into state. Roles with a `per_role` override (e.g. ceremonies on a
/// cheap model) get their own failover chain; everything else uses the default.
fn build_engine(
    config: &Config,
    logs_dir: PathBuf,
) -> Result<BuiltEngine, Box<dyn std::error::Error>> {
    let default = build_failover(&config.engine.default, &config.engine.fallbacks)?;
    let mut per_role = std::collections::HashMap::new();
    for (role, choice) in &config.engine.per_role {
        match build_failover(choice, &config.engine.fallbacks) {
            Ok(e) => {
                tracing::info!(
                    "role {role:?} routed to {:?}({})",
                    choice.engine,
                    choice.model
                );
                per_role.insert(*role, e);
            }
            Err(e) => tracing::warn!("per-role engine for {role:?} unavailable: {e}"),
        }
    }
    tracing::info!(
        "default engine {:?}({}) + {} fallback(s)",
        config.engine.default.engine,
        config.engine.default.model,
        config.engine.fallbacks.len()
    );
    let router = RoutingEngine::new(default, per_role);
    let logged = TranscriptEngine::new(router, logs_dir);
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
    // Worker identity for the shared registry + claim ownership. A headless
    // worker has no web login, so it takes its name from COXAGENT_OPERATOR.
    let operator = std::env::var("COXAGENT_OPERATOR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "worker".to_owned());
    let worker = format!("{operator}@{}", worker_host());
    // Isolate this worker's git checkout (when COXAGENT_WORKTREE is set) so
    // several workers can share one machine without racing on a single working
    // tree. On separate machines each already has its own clone, so this is a
    // no-op there.
    let work_dir = isolate_worktree(work_dir, &worker);
    // Forge for PRs/merges — so a headless `coxagent run` box is a full team
    // (commits, opens PRs, merges), not just a designer. Same wiring as the hub.
    let forge: Option<Arc<dyn coxagent_application::ports::outbound::ForgePort>> =
        if config.git.enabled && !config.git.repo.is_empty() {
            let (repo, base, wd) = (
                config.git.repo.clone(),
                config.git.base_url.clone(),
                work_dir.clone(),
            );
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
    let mut uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context)
        .with_meter(meter)
        .with_deploy(std::sync::Arc::new(DockerComposeDeploy::new()))
        .with_git(std::sync::Arc::new(
            coxagent_infrastructure::SystemGit::new(),
        ));
    if let Some(f) = forge {
        uc = uc.with_forge(f);
    }
    if let Some(url) = webhook.filter(|u| !u.is_empty()) {
        uc = uc.with_notifier(std::sync::Arc::new(WebhookNotifier::new(url)));
    }
    // Heartbeat the shared worker registry with the live role + ticket each phase,
    // so every dashboard shows this headless team's current agent.
    let hb_store = Arc::clone(&store);
    let hb_worker = worker.clone();
    uc.set_phase_reporter(std::sync::Arc::new(move |info| {
        // Beat the live role+ticket on a phase, "idle" between — never stale.
        let (role, note) = match info {
            Some((role, note)) => (role, note),
            None => ("idle".to_owned(), String::new()),
        };
        let (s, w) = (Arc::clone(&hb_store), hb_worker.clone());
        tokio::spawn(async move {
            let now = coxagent_application::state::now_rfc3339();
            let _ = s.heartbeat_worker(&w, &role, &note, &now).await;
        });
    }));
    uc.set_worker(worker);
    let shutdown = shutdown::Shutdown::listen();
    tracing::info!("cycle loop started");

    // A headless worker can auto-exit after N consecutive idle cycles (no work),
    // so it doesn't linger forever. 0/unset = run until stopped.
    let max_idle: u64 = std::env::var("COXAGENT_MAX_IDLE_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut idle = 0u64;

    let mut cycle = 0u64;
    while !shutdown.is_triggered() {
        cycle += 1;
        let report = uc.run_cycle(cycle).await;
        tracing::info!("{}", report.summary());
        for e in &report.errors {
            tracing::warn!("{e}");
        }
        if report.over_budget {
            tracing::warn!("budget/quota cap reached — stopping loop");
            break;
        }
        if report.did_work() {
            idle = 0;
        } else {
            idle += 1;
            if max_idle > 0 && idle >= max_idle {
                tracing::info!("idle for {idle} cycle(s) — auto-stopping worker");
                break;
            }
        }
        if max_cycles.is_some_and(|m| cycle >= m) {
            break;
        }
        shutdown.sleep_or_shutdown(sleep).await;
    }

    tracing::info!("cycle loop stopped after {cycle} cycle(s)");
    Ok(format!("stopped after {cycle} cycle(s)\n"))
}

/// This machine's hostname (the "machine" a headless worker runs on), or
/// `"local"`. Used to name the worker `operator@host` in the shared registry.
fn worker_host() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".to_owned())
}

/// When `COXAGENT_WORKTREE` is set and `work_dir` is a git repo, give this
/// worker its own detached git worktree (a sibling dir keyed by worker id) so
/// several workers on one machine never edit the same working tree at once. The
/// worktree shares the repo's objects/refs, so branches and pushes still land in
/// the same history. Returns the isolated path, or the original `work_dir` when
/// disabled or on any error (a best-effort convenience, never fatal).
fn isolate_worktree(work_dir: PathBuf, worker: &str) -> PathBuf {
    if std::env::var("COXAGENT_WORKTREE")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return work_dir;
    }
    let is_repo = std::process::Command::new("git")
        .arg("-C")
        .arg(&work_dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !is_repo {
        return work_dir;
    }
    let slug: String = worker
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Sibling of the repo, so it is never inside the tree the agent commits.
    let wt = work_dir
        .parent()
        .unwrap_or(&work_dir)
        .join(".coxagent-worktrees")
        .join(&slug);
    if wt.exists() {
        return wt;
    }
    let base = std::process::Command::new("git")
        .arg("-C")
        .arg(&work_dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "main".to_owned());
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(&work_dir)
        .args(["worktree", "add", "--detach"])
        .arg(&wt)
        .arg(&base)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        tracing::info!("worker checkout isolated at {}", wt.display());
        wt
    } else {
        work_dir
    }
}

/// Append one compression sample (`before after` bytes) to the shim dir's
/// savings log, so the dashboard can report the token-saver's effectiveness.
/// Best-effort and cheap; skips no-op passes and when no shim dir is set.
fn record_compression(before: usize, after: usize) {
    use std::io::Write as _;
    if before == 0 || after >= before {
        return;
    }
    let Ok(dir) = std::env::var("COXAGENT_SHIM_DIR") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::path::Path::new(&dir).join("savings.log"))
    {
        let _ = writeln!(f, "{before} {after}");
    }
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
