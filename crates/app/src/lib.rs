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
use std::time::Duration;

/// CLI entry shared by every service binary (coxagent, cox-gateway, …).
pub async fn cli_main() -> ExitCode {
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
            let (engine, _meter) = build_engine(&config, logs_dir(&args.state_dir), None)?;
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
pub fn load_coordination(base: &Path) {
    if std::env::var("COXAGENT_DB_DSN").is_ok_and(|v| !v.is_empty()) {
        return; // an explicit env always wins
    }
    let path = base.join("coordination.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    // Secrets hygiene: this file may carry credentials (DSNs). Clamp it to
    // owner-only and steer real deployments toward env/secret managers —
    // values support `${VAR}` interpolation so the file can stay secret-free.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                tracing::warn!(
                    "coordination.json was group/world-readable ({mode:o}) — tightened to 600. \
                     Prefer COXAGENT_DB_DSN/AUTH_DSN/REDIS_URL env vars (or ${{VAR}} \
                     placeholders in the file) over inline credentials."
                );
            }
        }
    }
    // `${VAR}` placeholders resolve from the environment at load time.
    let text = {
        let mut t = text;
        while let Some(start) = t.find("${") {
            let Some(end_rel) = t[start..].find('}') else {
                break;
            };
            let var = t[start + 2..start + end_rel].to_owned();
            let val = std::env::var(&var).unwrap_or_default();
            t.replace_range(start..=start + end_rel, &val);
        }
        t
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
    auth: Option<&Arc<dyn coxagent_application::auth::AuthPort>>,
) -> Result<coxagent_presentation::ProjectHandle, Box<dyn std::error::Error>> {
    use coxagent_application::use_cases::{run_forever, RunCycleUseCase, RunnerHandle};

    let store = make_store(id, state_dir).await?;
    let config = load_config(state_dir);
    // `auth` must be the SAME store the hub actually serves /api/mcp with —
    // NOT re-derived from state_dir here. Each project can live under a
    // different workspace root than the hub-wide auth.json (see run_hub's
    // registry), so minting against a re-opened, path-guessed auth store
    // would persist a token the real serving store never loads and every
    // MCP call would 401.
    let mcp = build_mcp_access(&config, auth, id, id).await;
    let (engine, meter) = build_engine(&config, logs_dir(state_dir), mcp)?;
    let sleep = std::time::Duration::from_secs(config.workflow.sleep_seconds);

    let recovered = RecoverUseCase::new(Arc::clone(&store)).execute().await?;
    if !recovered.is_empty() {
        tracing::info!("[{id}] recovered {} orphaned claim(s)", recovered.len());
    }

    let loaded = store.load().await.ok();
    let alias = loaded.as_ref().map(|s| s.alias.clone()).unwrap_or_default();
    let custom_name = loaded.as_ref().and_then(|s| s.display_name.clone());
    let mut context =
        std::fs::read_to_string(state_dir.join("project_context.md")).unwrap_or_default();
    // Prepend the company-wide conventions (set once in the Workspace screen) so
    // every agent on every project follows the same house rules.
    let hub_dir = state_dir.parent().and_then(Path::parent);
    if let Some(conv) = hub_dir.and_then(workspace_conventions) {
        if !conv.trim().is_empty() {
            context = format!("## Company conventions (apply to all work)\n{conv}\n\n{context}");
        }
    }
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
    let concurrency = config.workflow.concurrency.max(1);
    let handle = Arc::new(RunnerHandle::new());

    // Leader runner: singleton phases (BA, PO, standup, etc.)
    {
        let leader = RunCycleUseCase::new(
            Arc::clone(&store),
            engine.clone(),
            config.clone(),
            work_dir.clone(),
            context.clone(),
        )
        .with_meter(meter.clone())
        .with_live_budget(Arc::clone(&live_budget))
        .with_deploy(Arc::new(DockerComposeDeploy::new()))
        .with_git(Arc::new(coxagent_infrastructure::SystemGit::new()));
        let leader = if let Some(ref f) = forge {
            leader.with_forge(Arc::clone(f))
        } else {
            leader
        };
        let leader = leader.with_notifier(build_notifier(Arc::clone(&store), webhook.clone()));
        let wh = Arc::clone(&handle);
        tokio::spawn(async move { run_forever(wh, leader, sleep).await });
    }

    tracing::info!(
        "[{id}] spawning {} worker runner(s) (total {} runners)",
        concurrency.saturating_sub(1),
        concurrency
    );
    for _ in 1..concurrency {
        let worker = RunCycleUseCase::new(
            Arc::clone(&store),
            engine.clone(),
            config.clone(),
            work_dir.clone(),
            context.clone(),
        )
        .with_meter(meter.clone())
        .with_live_budget(Arc::clone(&live_budget))
        .with_deploy(Arc::new(DockerComposeDeploy::new()))
        .with_git(Arc::new(coxagent_infrastructure::SystemGit::new()));
        let worker = if let Some(ref f) = forge {
            worker.with_forge(Arc::clone(f))
        } else {
            worker
        };
        let worker = worker.with_notifier(build_notifier(Arc::clone(&store), webhook.clone()));
        let wh = Arc::clone(&handle);
        tokio::spawn(async move { run_forever(wh, worker, Duration::from_secs(5)).await });
    }

    // Auto-resume this machine's operator if the user left it running last time
    // (per-operator desired state). This restores only THIS user's operator —
    // it never starts anyone else's, so no one's credentials get spent for them.
    if let Ok(op) = std::env::var("COXAGENT_OPERATOR") {
        if !op.is_empty() {
            let operator = format!("{op}@{}", worker_host());
            if matches!(store.get_desired(&operator).await, Ok(Some(true))) {
                handle.set_operator(&op, &worker_host());
                handle.resume();
                tracing::info!("auto-resumed operator {operator} (left running)");
            }
        }
    }

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
    // Single-project serve honors the same RBAC env vars as the hub. Built
    // BEFORE build_project so the project's internal MCP token (see
    // build_mcp_access) mints against this exact store, not a re-derived one.
    let auth = build_auth(state_dir.parent().unwrap_or(state_dir)).await?;
    let project = build_project("default", state_dir, work_dir, auth.as_ref()).await?;
    let audit = build_audit().await;
    let extras = coxagent_presentation::HubExtras {
        auth,
        engines: detected_engines(),
        tooling: detected_tooling(),
        // System chat + its media live alongside the project state.
        hub_dir: Some(state_dir.parent().unwrap_or(state_dir).to_path_buf()),
        storage: build_storage().await,
        doc_store: build_doc_store().await,
        syschat_store: build_syschat_store(state_dir.parent().unwrap_or(state_dir)).await,
        ..Default::default()
    };
    coxagent_presentation::serve_full(vec![project], port, audit, extras).await?;
    Ok(())
}

/// Documentation store: MongoDB when `COXAGENT_MONGO_URL` is set, else `None`
/// (docs fall back to per-project `state.json`).
/// Shared KV store for hub-wide singletons (system chat), backed by Postgres
/// when a state DSN is configured. Falls back to `None` (local file) otherwise.
/// On first use it migrates an existing `system_chat.json` under `hub_dir` into
/// the database so nothing is lost when moving off the local file.
async fn build_syschat_store(
    hub_dir: &Path,
) -> Option<std::sync::Arc<dyn coxagent_application::ports::outbound::KvDocPort>> {
    use coxagent_application::ports::outbound::KvDocPort;
    let dsn = std::env::var("COXAGENT_DB_DSN")
        .ok()
        .filter(|s| !s.is_empty())?;
    match coxagent_infrastructure::PgKvDoc::connect(&dsn).await {
        Ok(store) => {
            // One-time migration: seed the DB from the local file if the DB has
            // no system-chat doc yet but a file exists.
            if matches!(store.load("system_chat").await, Ok(None)) {
                if let Ok(text) = std::fs::read_to_string(hub_dir.join("system_chat.json")) {
                    if !text.trim().is_empty() {
                        match store.save("system_chat", &text).await {
                            Ok(()) => {
                                tracing::info!("system chat: migrated local file into Postgres");
                            }
                            Err(e) => tracing::warn!("system chat migration failed: {e}"),
                        }
                    }
                }
            }
            tracing::info!("system chat store: Postgres");
            Some(std::sync::Arc::new(store))
        }
        Err(e) => {
            tracing::warn!("Postgres KV configured but unavailable ({e}); using local file");
            None
        }
    }
}

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

/// Entry for a dedicated runner service (cox-runner): one project's operator
/// loop, configured purely by env — 12-factor, no CLI parsing.
///
/// # Errors
/// Returns an error when the store can't be built or the loop fails fatally.
pub async fn operator_main(
    state_dir: std::path::PathBuf,
    work_dir: std::path::PathBuf,
    max_cycles: Option<u64>,
) -> Result<String, Box<dyn std::error::Error>> {
    load_coordination(
        state_dir
            .parent()
            .and_then(Path::parent)
            .unwrap_or(&state_dir),
    );
    let pid = state_dir.parent().and_then(Path::file_name).map_or_else(
        || "default".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    run_loop(
        make_store(&pid, &state_dir).await?,
        &state_dir,
        work_dir,
        String::new(),
        max_cycles,
    )
    .await
}

/// Serve many projects from a hub registry file — all roles, or the surface
/// selected by `COXAGENT_ROLE`. The registry is a JSON array of
/// `{ "id", "path" }` where `path` contains `state/` and `codebase/`.
///
/// # Errors
/// Returns an error when the registry can't be read or the port can't bind.
pub async fn run_hub(registry: &Path, mut port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // If the requested port is in use, scan upward for a free one so the hub
    // never fails to start — especially important when the Docker stack (which
    // uses port 4000 internally) and the desktop app share the same host.
    {
        let mut free = false;
        for _ in 0..50 {
            match std::net::TcpListener::bind(("127.0.0.1", port)) {
                Ok(_) => {
                    free = true;
                    break;
                }
                Err(_) => port += 1,
            }
        }
        if !free {
            return Err(format!("no free port found starting at {port}").into());
        }
    }
    tracing::info!("hub binding to port {port}");
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

    // Built BEFORE the per-project loop, and hub-wide (rooted at the
    // registry, not any one project's workspace) so every project's internal
    // MCP token (see build_mcp_access) mints against the SAME store this hub
    // actually serves auth from below — each project's own e.path is a
    // different directory than the registry's, so deriving auth per-project
    // here would silently mint tokens nobody validates against.
    let auth = build_auth(registry.parent().unwrap_or_else(|| Path::new("."))).await?;

    let mut projects = Vec::new();
    for e in entries {
        let state_dir = e.path.join("state");
        let work_dir = e.path.join("codebase");
        match build_project(&e.id, &state_dir, work_dir, auth.as_ref()).await {
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
        let auth = auth.clone();
        move |req| {
            let base = base.clone();
            let registry_path = registry_path.clone();
            let auth = auth.clone();
            Box::pin(
                async move { onboard_project(&base, &registry_path, req, auth.as_ref()).await },
            )
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
    // from opencode which supports any provider.
    let analyzer = build_engine(
        &Config {
            engine: coxagent_application::config::EngineMapping {
                default: coxagent_application::config::EngineChoice {
                    engine: coxagent_application::config::EngineKind::Opencode,
                    model: "bizbrain/DeepSeek-V4-Pro".to_owned(),
                },
                per_role: std::collections::HashMap::new(),
                fallbacks: Vec::new(),
                auto_fallback: true,
                escalation: Vec::new(),
            },
            git: Default::default(),
            workflow: Default::default(),
            architecture: Vec::new(),
            deploy: Default::default(),
            policy: Default::default(),
        },
        logs_dir(&base),
        None,
    )
    .ok()
    .map(|(e, _)| {
        let engine: Arc<dyn coxagent_application::ports::outbound::AgentEnginePort> = e;
        (engine, base.clone())
    });

    let audit = build_audit().await;
    let extras = coxagent_presentation::HubExtras {
        factory: Some(factory),
        remover: Some(remover),
        auth, // built above, before the per-project loop — see the comment there
        engines: detected_engines(),
        tooling: detected_tooling(),
        analyzer,
        // System-wide chat lives at the hub root (next to the registry).
        hub_dir: Some(base.clone()),
        storage: build_storage().await,
        doc_store: build_doc_store().await,
        syschat_store: build_syschat_store(&base).await,
    };
    coxagent_presentation::serve_full(projects, port, audit, extras).await?;
    Ok(())
}

/// Remove a project entry (by id) from the hub registry file (atomic via temp file rename).
fn remove_from_registry(registry_path: &Path, id: &str) -> Result<(), String> {
    let tmp = registry_path.with_extension("json.tmp");
    let text = std::fs::read_to_string(registry_path).map_err(|e| e.to_string())?;
    let mut arr: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    arr.retain(|e| e.get("id").and_then(|v| v.as_str()) != Some(id));
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&arr).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, registry_path).map_err(|e| e.to_string())
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

/// A minted API token, scoped read-only, that this operator's spawned agent
/// CLIs (claude/opencode) use to call the hub's own `/api/mcp` endpoint —
/// letting agents query the code graph live instead of only through the
/// static prompt text. Labeled `internal:mcp:<identity>` and filtered out of
/// the admin dashboard's token list (`list_tokens_ep`) — it's plumbing, not a
/// credential for a human to manage. Re-minted on every startup (the previous
/// one for this identity is revoked first, keeping auth.json from
/// accumulating dead entries across restarts); `None` when auth is disabled
/// (`/api/mcp` accepts unauthenticated loopback calls in that mode) or
/// minting fails.
///
/// Persisted (as a hash, like any other token) rather than held purely in
/// memory: the operator that spawns the agent CLI and the hub that serves
/// `/api/mcp` are not always the same OS process (see `Command::Run` vs
/// `Command::Serve`), so the credential has to be checkable by whichever
/// process is actually serving the request.
async fn ensure_internal_mcp_token(
    auth: &Arc<dyn coxagent_application::auth::AuthPort>,
    identity: &str,
) -> Option<String> {
    use coxagent_application::auth::AuthRole;
    let label = format!("internal:mcp:{identity}");
    auth.revoke_token(&label).await; // drop any stale token from a prior run
                                     // NOT AuthRole::Viewer: `auth_mw` (server.rs) gates every POST as a write
                                     // unless the path is explicitly exempted, and /api/mcp isn't (it's the
                                     // single JSON-RPC endpoint for both reads like search_symbols and writes
                                     // like report_blocker, so the middleware can't tell them apart from the
                                     // HTTP verb alone). Viewer.can_write() is false, so a Viewer-scoped
                                     // token would get 403'd on every call, including read-only ones. `Be` is
                                     // the lowest member-tier role that still satisfies can_write() — chosen
                                     // arbitrarily among the member tier, since none of them map naturally to
                                     // "the agent working this project" and MCP tool access doesn't
                                     // distinguish between member sub-roles.
    match auth.create_token(&label, AuthRole::Be).await {
        Some(secret) => Some(secret),
        None => {
            tracing::warn!("could not mint internal MCP token for {identity}");
            None
        }
    }
}

/// Build this run's loopback [`McpAccess`] for `project` (its id, as known to
/// the hub — goes on every MCP tool call), when the hub is expected to be
/// reachable on `config.deploy.host_port`. `token_identity` namespaces the
/// internal token label (see [`ensure_internal_mcp_token`]) — pass something
/// that's unique to the *process* minting it (e.g. including the operator
/// name), not just the project, so two operators working the same project
/// concurrently don't revoke each other's freshly minted token.
async fn build_mcp_access(
    config: &Config,
    auth: Option<&Arc<dyn coxagent_application::auth::AuthPort>>,
    project: &str,
    token_identity: &str,
) -> Option<coxagent_infrastructure::engine::McpAccess> {
    let port = config.deploy.host_port.unwrap_or(4000);
    let token = match auth {
        Some(auth) => ensure_internal_mcp_token(auth, token_identity).await,
        None => None, // open mode: /api/mcp accepts unauthenticated loopback calls
    };
    Some(coxagent_infrastructure::engine::McpAccess {
        url: format!("http://127.0.0.1:{port}/api/mcp"),
        token,
        project: project.to_owned(),
    })
}

/// Scaffold a new project workspace under `base`, seed it, append it to the hub
/// registry, and build a live [`ProjectHandle`]. Used by the dashboard's
/// "new project" flow.
async fn onboard_project(
    base: &Path,
    registry_path: &Path,
    req: coxagent_presentation::NewProjectReq,
    auth: Option<&Arc<dyn coxagent_application::auth::AuthPort>>,
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

    // Seed the confirmed project brief (from AI-assisted drafting), if any —
    // MERGED into the auto-drafted comprehension context, not overwriting it, so
    // the BA sees both the detected stack/structure and the user's goal.
    if let Some(goal) = req.goal.as_ref().filter(|g| !g.trim().is_empty()) {
        let ctx_path = state_dir.join("project_context.md");
        let base_ctx = std::fs::read_to_string(&ctx_path).unwrap_or_default();
        let merged = if base_ctx.trim().is_empty() {
            goal.clone()
        } else {
            format!(
                "{}\n\n## Goal (from onboarding)\n{goal}\n",
                base_ctx.trim_end()
            )
        };
        let _ = std::fs::write(&ctx_path, merged);
    }

    // Assign a unique host port so this project's `docker compose` deploy does
    // not clash with the others on this host.
    assign_host_port(base, registry_path, &proj_dir).map_err(|e| e.to_string())?;

    append_registry(registry_path, &id, &proj_dir).map_err(|e| e.to_string())?;

    build_project(&id, &state_dir, work_dir, auth)
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
                  // Also exclude ports published by Docker containers so a new project never
                  // picks a port already serving another app.
    if let Ok(out) = std::process::Command::new("docker")
        .args(["ps", "--format", "{{.Ports}}"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for pair in text.split_whitespace() {
            if let Some((host, _)) = pair.split_once("->") {
                if let Some((_, hp)) = host.rsplit_once(':') {
                    if let Ok(p) = hp.parse::<u16>() {
                        used.insert(p);
                    }
                }
            }
        }
    }
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

/// Append `{ id, path }` to the hub registry JSON array (atomic via temp file rename).
fn append_registry(registry_path: &Path, id: &str, path: &Path) -> std::io::Result<()> {
    let tmp = registry_path.with_extension("json.tmp");
    let text = std::fs::read_to_string(registry_path).unwrap_or_else(|_| "[]".to_owned());
    let mut arr: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    arr.push(serde_json::json!({ "id": id, "path": path }));
    std::fs::write(&tmp, serde_json::to_string_pretty(&arr)?)?;
    std::fs::rename(&tmp, registry_path)
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
    mcp: Option<&coxagent_infrastructure::engine::McpAccess>,
    escalation: &[String],
    sandbox: bool,
) -> Result<FailoverEngine<AnyEngine>, Box<dyn std::error::Error>> {
    let mut engines = vec![AnyEngine::from_choice_with_escalation(
        choice,
        mcp.cloned(),
        escalation,
        sandbox,
    )?];
    for fb in fallbacks {
        match AnyEngine::from_choice(fb, mcp.cloned()) {
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
/// The fallback chain actually used: the explicit `fallbacks` first, then — when
/// `auto_fallback` is on (the default) — a cheaper same-CLI tier and every other
/// agent CLI detected on this host, so failover works out of the box and the user
/// only flips one switch instead of listing models by hand.
fn effective_fallbacks(config: &Config) -> Vec<coxagent_application::config::EngineChoice> {
    use coxagent_application::config::{EngineChoice, EngineKind};
    let mut out = config.engine.fallbacks.clone();
    if !config.engine.auto_fallback {
        return out;
    }
    let default_kind = config.engine.default.engine;
    let mut push = |kind: EngineKind, model: String| {
        if !out.iter().any(|c| c.engine == kind && c.model == model) {
            out.push(EngineChoice {
                engine: kind,
                model,
            });
        }
    };
    // Cheap same-CLI tier: haiku for Claude, a lightweight model for opencode.
    match default_kind {
        EngineKind::Claude if config.engine.default.model != "haiku" => {
            push(EngineKind::Claude, "haiku".to_owned());
        }
        EngineKind::Opencode
            if config.engine.default.model != "bizbrain/Qwen3.6-35B-A3B-thinking" =>
        {
            push(
                EngineKind::Opencode,
                "bizbrain/Qwen3.6-35B-A3B-thinking".to_owned(),
            );
        }
        _ => {}
    }
    // Every other installed CLI as a backup engine.
    for d in discover() {
        if d.kind == default_kind {
            continue;
        }
        let model = match d.kind {
            EngineKind::Claude => "haiku".to_owned(),
            EngineKind::Opencode => "bizbrain/Qwen3.6-35B-A3B-thinking".to_owned(),
            EngineKind::Hermes => "hermes-3-llama-3.2-3b".to_owned(),
            _ => continue,
        };
        push(d.kind, model);
    }
    out
}

fn build_engine(
    config: &Config,
    logs_dir: PathBuf,
    mcp: Option<coxagent_infrastructure::engine::McpAccess>,
) -> Result<BuiltEngine, Box<dyn std::error::Error>> {
    if config.workflow.sandbox && !cfg!(target_os = "macos") {
        tracing::warn!(
            "workflow.sandbox is on but this platform has no sandbox backend yet — agents run unsandboxed"
        );
    }
    let fallbacks = effective_fallbacks(config);
    let default = build_failover(
        &config.engine.default,
        &fallbacks,
        mcp.as_ref(),
        &config.engine.escalation,
        config.workflow.sandbox,
    )?;
    let mut per_role = std::collections::HashMap::new();
    for (role, choice) in &config.engine.per_role {
        match build_failover(
            choice,
            &fallbacks,
            mcp.as_ref(),
            &config.engine.escalation,
            config.workflow.sandbox,
        ) {
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
        "default engine {:?}({}) + {} fallback(s){}",
        config.engine.default.engine,
        config.engine.default.model,
        fallbacks.len(),
        if config.engine.auto_fallback {
            " [auto]"
        } else {
            ""
        }
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
#[allow(clippy::too_many_lines)] // linear setup + loop; splitting hurts readability
async fn run_loop(
    store: Arc<AnyStateStore>,
    state_dir: &Path,
    work_dir: PathBuf,
    context: String,
    max_cycles: Option<u64>,
) -> Result<String, Box<dyn std::error::Error>> {
    let config = load_config(state_dir);
    // Same project-id derivation as Command::Run/operator_main: the workspace
    // dir name (e.g. `cxc`), not a fixed "default" — so this operator's MCP
    // calls target the same project the hub knows it by.
    let pid = state_dir.parent().and_then(Path::file_name).map_or_else(
        || "default".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mcp_operator = std::env::var("COXAGENT_OPERATOR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "worker".to_owned());
    // A headless `coxagent run` has no hub of its own — it's a standalone
    // process, so (unlike build_project's callers) it self-derives auth here
    // rather than receiving an already-built store. This is only correct
    // because `--state-dir` is conventionally the SAME workspace path the
    // hub was started with (see Command::Run's doc comment on `pid`), so
    // this resolves to the same auth.json the hub actually serves from.
    let auth = build_auth(state_dir.parent().unwrap_or(state_dir))
        .await
        .ok()
        .flatten();
    let mcp = build_mcp_access(
        &config,
        auth.as_ref(),
        &pid,
        &format!("{pid}:{mcp_operator}"),
    )
    .await;
    let (engine, meter) = build_engine(&config, logs_dir(state_dir), mcp)?;
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
    // Ship this operator's live logs to shared storage (MinIO) so the central
    // hub can show a remote operator's live agent log, not just local ones.
    spawn_log_uploader(state_dir, &work_dir);
    let mut uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context)
        .with_meter(meter)
        .with_deploy(std::sync::Arc::new(DockerComposeDeploy::new()))
        .with_git(std::sync::Arc::new(
            coxagent_infrastructure::SystemGit::new(),
        ));
    if let Some(f) = forge {
        uc = uc.with_forge(f);
    }
    uc = uc.with_notifier(build_notifier(Arc::clone(&store), webhook));
    // Heartbeat the shared worker registry with the live role + ticket each phase,
    // so every dashboard shows this headless team's current agent.
    let hb_store = Arc::clone(&store);
    let hb_worker = worker.clone();
    // Shared live phase + keepalive: a single engine call can run for tens of
    // minutes while the registry TTL is a few minutes, so without a mid-phase
    // refresh a busy operator would drop off the dashboard and look dead.
    let phase: Arc<Mutex<(String, String)>> =
        Arc::new(Mutex::new(("idle".to_owned(), String::new())));
    {
        let (s, w, phase) = (Arc::clone(&store), hb_worker.clone(), Arc::clone(&phase));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(45)).await;
                let (role, note) = phase
                    .lock()
                    .map_or_else(|_| ("idle".to_owned(), String::new()), |p| p.clone());
                if role != "idle" {
                    let now = coxagent_application::state::now_rfc3339();
                    let _ = s.heartbeat_worker(&w, &role, &note, &now).await;
                }
            }
        });
    }
    uc.set_phase_reporter(std::sync::Arc::new(move |info| {
        // Beat the live role+ticket on a phase, "idle" between — never stale.
        let (role, note) = match info {
            Some((role, note)) => (role, note),
            None => ("idle".to_owned(), String::new()),
        };
        if let Ok(mut p) = phase.lock() {
            *p = (role.clone(), note.clone());
        }
        let (s, w) = (Arc::clone(&hb_store), hb_worker.clone());
        tokio::spawn(async move {
            let now = coxagent_application::state::now_rfc3339();
            let _ = s.heartbeat_worker(&w, &role, &note, &now).await;
        });
    }));
    let operator = worker.clone();
    uc.set_worker(worker);

    // Single-instance lock: refuse to start a second process for the same
    // `operator@host`. Two runners under one identity share a claim owner,
    // clobber each other's registry heartbeat, and double the token spend —
    // exactly the "two luffy" duplication. The lock is held (and renewed) by
    // this process's PID; a duplicate only wins once the holder dies.
    let instance = std::process::id().to_string();
    if !store
        .acquire_operator(&operator, &instance)
        .await
        .unwrap_or(true)
    {
        tracing::warn!(
            "operator {operator} is already running on this host — not starting a duplicate"
        );
        return Ok(format!(
            "operator {operator} already active elsewhere; refusing to start a duplicate\n"
        ));
    }
    {
        let (s, op, inst) = (Arc::clone(&store), operator.clone(), instance.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let _ = s.acquire_operator(&op, &inst).await; // renew our hold
            }
        });
    }

    let shutdown = shutdown::Shutdown::listen();
    tracing::info!("cycle loop started");

    // A headless worker can auto-exit after N consecutive idle cycles (no work),
    // so it doesn't linger forever. 0/unset = run until stopped.
    let max_idle: u64 = std::env::var("COXAGENT_MAX_IDLE_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut idle = 0u64;

    // An app-spawned operator waits for an explicit web Start before doing any
    // work (so opening the app never silently burns tokens); a manually launched
    // `cox-server run` keeps its run-immediately default.
    let wait_for_start = std::env::var("COXAGENT_WAIT_FOR_START").is_ok_and(|v| v == "1");

    // Fast job poll: a human's force-merge queued by the hub starts within
    // ~15s on this runner instead of waiting for the next full cycle.
    let uc = Arc::new(uc);
    {
        let uc = Arc::clone(&uc);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                uc.drain_jobs().await;
            }
        });
    }

    let mut cycle = 0u64;
    while !shutdown.is_triggered() {
        // Honour this operator's per-user Start/Stop from the web: idle (without
        // exiting) when the user has stopped it — or, for an app-spawned operator,
        // until they first Start it — so no one's credentials are spent unbidden.
        let idle_now = match store.get_desired(&operator).await {
            Ok(Some(true)) => false,
            Ok(Some(false)) => true,
            _ => wait_for_start,
        };
        if idle_now {
            shutdown.sleep_or_shutdown(sleep).await;
            continue;
        }
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

/// Periodically mirror a headless operator's `logs/live/*.log` to shared storage
/// (MinIO) under `agentlogs/<project>/<file>`, so the central hub can serve a
/// remote operator's live log. No-op when S3 is not configured (single machine —
/// the hub reads the local files directly).
fn spawn_log_uploader(state_dir: &Path, work_dir: &Path) {
    use coxagent_application::ports::outbound::StoragePort;
    let Some(s3) = coxagent_infrastructure::S3Storage::from_env() else {
        return;
    };
    let project = state_dir.parent().and_then(Path::file_name).map_or_else(
        || "default".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let live_dir = work_dir
        .parent()
        .unwrap_or(work_dir)
        .join("logs")
        .join("live");
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            let Ok(entries) = std::fs::read_dir(&live_dir) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().is_some_and(|x| x == "log") {
                    if let Ok(data) = std::fs::read(&path) {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let key = format!("agentlogs/{project}/{name}");
                        let _ = s3.put(&key, &data, "text/plain").await;
                    }
                }
            }
        }
    });
}

/// The event notifier for a runner: always the project's own team chat (with a
/// native push via the app's chat-notification path), plus an external webhook
/// when one is configured.
fn build_notifier(
    store: Arc<AnyStateStore>,
    webhook: Option<String>,
) -> Arc<dyn coxagent_application::ports::outbound::NotifierPort> {
    use coxagent_application::ports::outbound::{ChatNotifier, FanoutNotifier, NotifierPort};
    let mut sinks: Vec<Arc<dyn NotifierPort>> = vec![Arc::new(ChatNotifier::new(store))];
    if let Some(url) = webhook.filter(|u| !u.is_empty()) {
        sinks.push(Arc::new(WebhookNotifier::new(url)));
    }
    Arc::new(FanoutNotifier(sinks))
}

/// The company-wide conventions from the workspace doc (`app_kv` key `workspace`
/// on Postgres, else `<hub>/workspace.json`). Best-effort; `None` when unset.
fn workspace_conventions(hub_dir: &Path) -> Option<String> {
    use coxagent_application::ports::outbound::KvDocPort;
    let raw = match std::env::var("COXAGENT_DB_DSN")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(dsn) => {
            // A short blocking read on its own runtime — build_project runs at
            // startup, so this one-off is fine and keeps the signature sync.
            std::thread::spawn(move || {
                tokio::runtime::Runtime::new().ok().and_then(|rt| {
                    rt.block_on(async {
                        coxagent_infrastructure::PgKvDoc::connect(&dsn)
                            .await
                            .ok()?
                            .load("workspace")
                            .await
                            .ok()
                            .flatten()
                    })
                })
            })
            .join()
            .ok()
            .flatten()?
        }
        None => std::fs::read_to_string(hub_dir.join("workspace.json")).ok()?,
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()?
        .get("conventions")?
        .as_str()
        .map(str::to_owned)
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
        // ONE write_all per line: many shim processes append concurrently, and
        // writeln! can split its write — torn lines glued two records together
        // and wrecked the stats. O_APPEND + a single small write is atomic.
        let _ = f.write_all(format!("{before} {after}\n").as_bytes());
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

/// Real-server regression coverage for the bug caught in review: `/api/mcp`
/// is a POST route, and `auth_mw` (presentation/src/server.rs) treats every
/// POST as a write unless the path is explicitly exempted — it isn't. A
/// `Viewer`-scoped token (can_write() == false) therefore gets 403'd on
/// EVERY MCP call, including read-only ones like `search_symbols`, which is
/// exactly why `ensure_internal_mcp_token` mints `AuthRole::Be` and not
/// `Viewer`. These tests boot the real hub (real router, real `auth_mw`, real
/// `FileAuthService`) and hit `/api/mcp` over real HTTP — no mocking of the
/// auth gate — so a regression here fails loudly instead of silently
/// 403-ing agents in production.
#[cfg(test)]
mod mcp_auth_tests {
    use coxagent_application::auth::{AuthPort, AuthRole};
    use coxagent_infrastructure::{FileAuthService, MemoryAuditSink};
    use std::sync::Arc;
    use std::time::Duration;

    /// Boots a real `serve_full` hub with RBAC on (one bootstrapped admin,
    /// two extra tokens minted), waits for `/api/health` to answer, and
    /// returns the port, the two tokens under test, and the backing tempdir
    /// (kept alive for the test's duration — `serve_full` writes hub-state
    /// backups under it).
    async fn boot_hub_with_tokens() -> (u16, String, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let auth_path = FileAuthService::default_path(dir.path());
        FileAuthService::bootstrap_admin(&auth_path, "admin", "adminpassword1")
            .expect("bootstrap admin");
        let svc = FileAuthService::open(&auth_path).expect("open auth store");
        let be_token = svc
            .create_token("test:be", AuthRole::Be)
            .await
            .expect("mint Be token");
        let viewer_token = svc
            .create_token("test:viewer", AuthRole::Viewer)
            .await
            .expect("mint Viewer token");
        let auth: Arc<dyn AuthPort> = Arc::new(svc);

        let port = 47_654;
        let extras = coxagent_presentation::HubExtras {
            auth: Some(auth),
            hub_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
            Arc::new(MemoryAuditSink::default());
        tokio::spawn(coxagent_presentation::serve_full(
            vec![],
            port,
            audit,
            extras,
        ));

        let client = reqwest::Client::new();
        let health_url = format!("http://127.0.0.1:{port}/api/health");
        for _ in 0..50 {
            if client.get(&health_url).send().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        (port, be_token, viewer_token, dir)
    }

    /// Minimal JSON-RPC `tools/list` — needs no project, so it isolates the
    /// auth gate from any project-lookup behavior.
    fn tools_list_body() -> serde_json::Value {
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })
    }

    // These three tests share one hub instance on a fixed port (real TCP
    // bind), so they run as one #[tokio::test] rather than three parallel
    // ones that would race on the same port.
    #[tokio::test]
    async fn api_mcp_auth_gate_matches_can_write_not_viewer() {
        let (port, be_token, viewer_token, _dir) = boot_hub_with_tokens().await;
        let url = format!("http://127.0.0.1:{port}/api/mcp");
        let client = reqwest::Client::new();

        // No credential at all: /api/mcp isn't in auth_mw's public-path
        // allowlist, so this must be rejected, not silently allowed.
        let resp = client
            .post(&url)
            .json(&tools_list_body())
            .send()
            .await
            .expect("request");
        assert_eq!(
            resp.status(),
            401,
            "unauthenticated /api/mcp should be rejected"
        );

        // Be (member tier, can_write() == true): must be allowed through,
        // including this read-only tools/list call.
        let resp = client
            .post(&url)
            .bearer_auth(&be_token)
            .json(&tools_list_body())
            .send()
            .await
            .expect("request");
        assert_eq!(
            resp.status(),
            200,
            "a can_write() role must reach mcp_ep, even for a read-only tool"
        );
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert!(
            body["result"]["tools"].is_array(),
            "expected a tools/list result, got: {body}"
        );

        // Viewer (can_write() == false): this is the exact bug caught in
        // review — assert it stays blocked by the write gate, so if someone
        // "fixes" the internal token back to Viewer, this test fails.
        let resp = client
            .post(&url)
            .bearer_auth(&viewer_token)
            .json(&tools_list_body())
            .send()
            .await
            .expect("request");
        assert_eq!(
            resp.status(),
            403,
            "Viewer can't write, so auth_mw blocks POST /api/mcp — this is \
             WHY ensure_internal_mcp_token must not use AuthRole::Viewer"
        );
    }
}

/// The one thing no fake-binary test can prove: that the REAL `claude` CLI,
/// given a real `--mcp-config` file, actually calls the tool instead of
/// ignoring it. `#[ignore]`d — costs a real API call and needs `claude`
/// logged in — run explicitly with `cargo test -- --ignored
/// live_claude_actually_calls_mcp_search_symbols`.
///
/// Scoped deliberately narrow: this indexes THIS repo (already built via
/// `coxagent codegraph build`) as a project and asks a read-only question —
/// it does not run a real DEV cycle, so there's nothing here that edits or
/// commits to the repo the model is looking at.
#[cfg(test)]
mod live_claude_mcp_test {
    use coxagent_infrastructure::engine::{ClaudeEngine, McpAccess};
    use coxagent_infrastructure::MemoryAuditSink;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    #[ignore]
    async fn live_claude_actually_calls_mcp_search_symbols() {
        use coxagent_application::ports::outbound::{AgentEnginePort, AgentRequest};
        use coxagent_domain::Role;

        let repo_root = std::path::PathBuf::from("/Users/steverogers/Projects/CoXAgent");
        let state_dir = std::env::temp_dir().join(format!("live-mcp-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).expect("mkdir");

        let project = super::build_project("cxc", &state_dir, repo_root, None)
            .await
            .expect("build_project against this repo");
        let port = 47_655;
        let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
            Arc::new(MemoryAuditSink::default());
        let extras = coxagent_presentation::HubExtras::default(); // open mode, no token needed
        tokio::spawn(coxagent_presentation::serve_full(
            vec![project],
            port,
            audit,
            extras,
        ));
        let client = reqwest::Client::new();
        let health_url = format!("http://127.0.0.1:{port}/api/health");
        for _ in 0..50 {
            if client.get(&health_url).send().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let engine = ClaudeEngine::new("sonnet").with_mcp(Some(McpAccess {
            url: format!("http://127.0.0.1:{port}/api/mcp"),
            token: None,
            project: "cxc".to_owned(),
        }));

        let outcome = engine
            .run(AgentRequest {
                role: Role::DevFeature,
                system_prompt: "You are a careful, minimal-tool-call code assistant.".to_owned(),
                task_prompt: "Use the search_symbols MCP tool (project=\"cxc\") to find the \
                    symbol `ensure_internal_mcp_token`. Report back which file and line it's \
                    defined in, in one sentence. Do NOT read, write, or edit any files, and do \
                    NOT run any shell commands — only use the MCP tool, then answer from its \
                    result."
                    .to_owned(),
                work_dir: std::path::PathBuf::from("/Users/steverogers/Projects/CoXAgent"),
                timeout: Duration::from_secs(120),
                escalation_level: 0,
            })
            .await
            .expect("claude run");

        println!("--- stdout ---\n{}", outcome.stdout);
        println!("--- trace ---\n{}", outcome.trace);
        assert!(outcome.succeeded(), "stderr: {}", outcome.stderr);
        assert!(
            outcome.trace.contains("search_symbols"),
            "expected a search_symbols MCP tool call in the trace, got:\n{}",
            outcome.trace
        );
    }
}
