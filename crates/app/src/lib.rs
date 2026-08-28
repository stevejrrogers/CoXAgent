//! CoXAgent composition root — the ONE place dependency injection happens.
//!
//! Parses the CLI, constructs the concrete adapters, and dispatches to the
//! application use cases: report, engine discovery, one-shot BA, the continuous
//! cycle loop (with graceful shutdown), and greenfield onboarding.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod onboard;
mod shutdown;

use coxagent_application::config::{
    Config, DeployConfig, GitConfig, PolicyConfig, ReleasesConfig, WorkflowConfig,
};
use coxagent_application::ports::outbound::{SandboxStatus, StateStorePort};
use coxagent_application::use_cases::{RecoverUseCase, RunBaUseCase, RunCycleUseCase};
use coxagent_application::Spend;
use coxagent_infrastructure::engine::{
    AnyEngine, FailoverEngine, Meter, MeteringEngine, RoutingEngine, TranscriptEngine,
};
use coxagent_infrastructure::{
    discover, AnyStateStore, DockerComposeDeploy, JsonStateStore, RestConfig, RestStateStore,
    SqlStateStore, WebhookNotifier,
};
use coxagent_presentation::{cli, render_changelog, render_report, Command};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

mod builders;
mod config_load;
mod host_port;
mod shims;

pub use builders::load_coordination;
#[allow(clippy::wildcard_imports)] // one module, many files — see builders.rs
use builders::*;
#[allow(clippy::wildcard_imports)] // one module, many files — see config_load.rs
use config_load::*;
#[allow(clippy::wildcard_imports)] // one module, many files — see host_port.rs
use host_port::*;
pub use shims::shim_script;
#[allow(clippy::wildcard_imports)] // one module, many files — see shims.rs
use shims::*;

/// CLI entry shared by every service binary (coxagent, cox-gateway, …).
pub async fn cli_main() -> ExitCode {
    init_tracing();
    // Boxed: the CLI dispatch future carries every command's locals, so keeping
    // it off the caller's stack matters more than one allocation per process.
    match Box::pin(run()).await {
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

pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}

/// The project id a `--state-dir` belongs to: the workspace directory name
/// (`~/CoXAgent/cxa/state` -> `cxa`), falling back to `default`.
///
/// Every single-project command must agree on this, or two of them address
/// different rows of the same shared Postgres for the same workspace.
fn project_id_for(state_dir: &Path) -> String {
    state_dir.parent().and_then(Path::file_name).map_or_else(
        || "default".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[allow(clippy::too_many_lines)] // a flat CLI-command dispatch; splitting hurts readability
async fn run() -> Result<String, Box<dyn std::error::Error>> {
    let args = cli::parse();
    // The single-project store is built lazily: `serve`/`hub`/`discover` don't
    // use it, so we must not create it eagerly — the default `./state` would
    // resolve against a read-only cwd (e.g. a GUI-launched app runs in `/`).
    //
    // The id is the workspace directory name, the SAME derivation `run` uses.
    // It was hardcoded "default" here, so on a shared Postgres `onboard
    // --state-dir ~/CoXAgent/cxa/state` wrote the new project into the `default`
    // row while the operator for that very workspace read `cxa`: one dashboard
    // entry holding the real project under the wrong name, and a second, empty
    // one that looked like a duplicate.
    let pid = project_id_for(&args.state_dir);
    let store = || make_store(&pid, &args.state_dir);

    match args.command {
        Command::Report => {
            let state = store().await?.load().await?;
            Ok(render_report(&state))
        }
        Command::Discover => Ok(render_discovery()),
        Command::Probe { hub, project } => run_probe(&hub, &project).await,
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
            let config = load_config(&args.state_dir)?;
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
            let config = load_config(&args.state_dir)?;
            let uc = coxagent_application::use_cases::RunConformanceUseCase::new(
                Arc::clone(&store),
                work_dir,
                config.architecture,
            )
            .with_files(Some(Arc::new(
                coxagent_infrastructure::FsWorkspaceFiles::new(),
            )));
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
            // `COXAGENT_PORT` exists so a build of THIS project, run by an agent
            // to try it out, does not land on the hub's port. Hunting a hub that
            // silently moved because its own dogfood build took 4000 is an hour
            // nobody gets back.
            let port = std::env::var("COXAGENT_PORT")
                .ok()
                .and_then(|v| v.trim().parse::<u16>().ok())
                .unwrap_or(port);
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
            let co_base = args
                .state_dir
                .parent()
                .and_then(Path::parent)
                .unwrap_or(&args.state_dir);
            // Shared coordination backend from a persistent file next to state
            // (Finder launch needs no env); then feed any locally-persisted
            // remote-store bearer so /store calls authenticate without hand-copy.
            load_coordination(co_base);
            provision_local_token(co_base);
            Box::pin(run_loop(
                make_store(&pid, &args.state_dir).await?,
                &args.state_dir,
                work_dir,
                context,
                max_cycles,
            ))
            .await
        }
        Command::Codegraph { query, work_dir } => codegraph_query(&work_dir, &query).await,
        Command::Compress {
            cmd,
            exact_check,
            args,
        } => {
            use std::io::{Read as _, Write as _};
            // git content-retrieval subcommands (show/diff/log/cat-file/...)
            // can emit raw file content — dedupe/clip would silently mutate
            // it, so pass those through byte-exact instead of compressing.
            let needs_exact = cmd.as_deref() == Some("git")
                && coxagent_application::tokens::git_needs_exact_output(&args);
            // Query mode for the shim: answer and exit without touching stdin,
            // so the shim can run the real command unpiped (native stdout,
            // stderr and exit code) instead of merging the streams.
            if exact_check {
                return Ok(if needs_exact {
                    "exact".to_owned()
                } else {
                    String::new()
                });
            }
            // Bytes, not a String: the wrapped command's output is whatever it
            // emitted. `read_to_string` rejects non-UTF-8 wholesale and leaves
            // the buffer empty, which turned `git show HEAD:logo.png` and
            // `git archive` into silent zero-byte results.
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).ok();
            // Non-UTF-8 output is passed through for the same reason: the
            // compressor works on lines and chars and cannot round-trip bytes.
            let out = match std::str::from_utf8(&input) {
                Ok(text) if !needs_exact => std::borrow::Cow::Owned(
                    coxagent_application::tokens::proxy_compress(text).into_bytes(),
                ),
                _ => std::borrow::Cow::Borrowed(input.as_slice()),
            };
            // Record the saving so the dashboard can show how effective the
            // token-saver is (appends "before after" to the shim dir's log).
            record_compression(input.len(), out.len());
            // Written here rather than returned: the payload is arbitrary bytes
            // and `cli_main` prints a `String`.
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(&out)?;
            stdout.flush()?;
            Ok(String::new())
        }
    }
}

/// Write the command-output shims (rtk-style) into a temp dir and return it.
/// Each shim runs the real command and pipes its output through
/// `coxagent compress` (only for non-tty, large output — small/exact output is
/// untouched). Applied to agent subprocesses only, so the hub's own tooling is
/// never affected.
/// Verbose, output-heavy commands worth compressing. `git` is included: the
/// small-output passthrough keeps porcelain (rev-parse/status) exact, and
/// `git_needs_exact_output` bypasses compression for content-retrieval
/// subcommands (show/diff/log/cat-file/...) so file content is never
/// dedupe'd or clipped.
const SHIM_CMDS: &[&str] = &[
    "cargo", "npm", "pnpm", "yarn", "pip", "pip3", "pytest", "go", "gradle", "mvn", "make",
    "docker", "git", "node", "python", "python3", "tsc", "jest", "vitest",
];

/// Commands whose argv can select byte-exact output. Their shim asks the
/// binary before piping, so `git_needs_exact_output` stays the single source
/// of truth instead of being re-implemented in shell. Only these pay the extra
/// process; every other shim keeps the plain pipeline.
const EXACT_AWARE_CMDS: &[&str] = &["git"];

#[cfg(test)]
mod project_id_tests {
    use super::project_id_for;
    use std::path::Path;

    /// Every single-project command must land on the SAME Postgres row for a
    /// given workspace. `onboard` hardcoded "default" while `run` derived the
    /// name, so onboarding `~/CoXAgent/cxa` wrote the project into `default`
    /// and the operator for it then read an empty `cxa` — the dashboard showed
    /// two projects, neither of them right.
    #[test]
    fn the_id_is_the_workspace_directory_name() {
        assert_eq!(
            project_id_for(Path::new("/Users/u/CoXAgent/cxa/state")),
            "cxa"
        );
        assert_eq!(
            project_id_for(Path::new("/srv/work/lynx-3/state")),
            "lynx-3"
        );
    }

    #[test]
    fn a_bare_state_dir_falls_back_to_default() {
        // `coxagent --state-dir state` from a workspace root: no parent name to
        // take, and "default" is the id a single-project install already uses.
        assert_eq!(project_id_for(Path::new("state")), "default");
    }
}

#[cfg(test)]
mod shim_script_tests {
    use super::{shim_script, SHIM_CMDS};

    /// COX-B015: the shim must forward the wrapped command's argv to
    /// `compress`, otherwise `git_needs_exact_output` can never see which
    /// subcommand ran and `git show`/`diff` output gets clipped to nonsense.
    #[test]
    fn shim_script_forwards_the_wrapped_argv_to_compress() {
        for cmd in SHIM_CMDS {
            let script = shim_script(cmd, "/tmp/coxagent-shims", "/opt/coxagent");
            let pipe = script
                .lines()
                // Skip the `--exact-check` probe — it is a query, not the pipe.
                .find(|l| l.contains("compress") && !l.contains("--exact-check"))
                .unwrap_or_else(|| panic!("{cmd} shim never pipes into compress"));
            assert!(
                pipe.contains(r#""/opt/coxagent" compress --cmd "$cmd" -- "$@""#),
                "{cmd} shim drops the wrapped argv: {pipe}"
            );
        }
    }

    /// COX-B015: the git shim must decide *before* the `2>&1` pipeline, and
    /// bypass it entirely, so a content subcommand's streams stay native.
    #[test]
    fn git_shim_checks_exactness_before_the_merging_pipeline() {
        let script = shim_script("git", "/tmp/coxagent-shims", "/opt/coxagent");
        let probe = script
            .find("--exact-check")
            .expect("git shim never asks whether the output must be exact");
        let bypass = script
            .find(r#"[ "$_exact" = "exact" ] && exec "$real" "$@""#)
            .expect("git shim never execs the real binary for exact output");
        let merge = script
            .find("2>&1")
            .expect("shim lost its compress pipeline");
        assert!(
            probe < bypass && bypass < merge,
            "the exactness bypass must come before the stderr merge:\n{script}"
        );
    }

    /// …and every other shim keeps the plain pipeline: no extra process, and
    /// compression of non-git commands is untouched.
    #[test]
    fn only_the_exact_aware_shims_pay_for_the_probe() {
        for cmd in SHIM_CMDS.iter().filter(|c| **c != "git") {
            let script = shim_script(cmd, "/tmp/coxagent-shims", "/opt/coxagent");
            assert!(
                !script.contains("--exact-check"),
                "{cmd} shim spawns a needless exactness probe"
            );
        }
    }

    /// The shim must not find itself when it resolves the real binary.
    #[test]
    fn shim_script_skips_its_own_directory_on_path() {
        let script = shim_script("git", "/tmp/coxagent-shims", "/opt/coxagent");
        assert!(script.contains(r#"[ "$d" = "/tmp/coxagent-shims" ] && continue"#));
    }
}

/// Answer a code-graph query for agents (and humans) — structured, token-cheap
/// output instead of grepping the tree by hand.
async fn codegraph_query(
    work_dir: &Path,
    query: &coxagent_presentation::CodegraphQuery,
) -> Result<String, Box<dyn std::error::Error>> {
    use coxagent_application::codegraph::{references, CodeGraph};
    use coxagent_presentation::CodegraphQuery as Q;
    use std::fmt::Write as _;

    let files = coxagent_infrastructure::FsWorkspaceFiles::new();
    // Build fresh for `build`; otherwise use the persisted index (build if absent).
    let graph = || async {
        match CodeGraph::load(&files, work_dir).await {
            Some(g) => g,
            None => CodeGraph::index(&files, work_dir).await,
        }
    };
    let mut out = String::new();
    match query {
        Q::Build => {
            let g = CodeGraph::index(&files, work_dir).await;
            g.save(&files, work_dir).await?;
            let _ = writeln!(
                out,
                "indexed {} files, {} symbols, {} calls",
                g.files.len(),
                g.symbols.len(),
                g.calls.len()
            );
        }
        Q::Search { query: q } => {
            let g = graph().await;
            for s in g.relevance_search(q, 50) {
                let scope = s
                    .scope
                    .as_deref()
                    .map_or(String::new(), |sc| format!("{sc}::"));
                let _ = writeln!(out, "{} {scope}{}  {}:{}", s.kind, s.name, s.file, s.line);
            }
        }
        Q::Impact { name } => {
            let refs = references(&files, work_dir, name, 200).await;
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
            let g = graph().await;
            let callers = g.callers(name);
            if callers.is_empty() {
                let _ = writeln!(out, "no callers found for `{name}`");
            }
            for (who, file, line) in callers {
                let _ = writeln!(out, "{who}  {file}:{line}");
            }
        }
        Q::Deps { file } => {
            let g = graph().await;
            for f in g.dependents(file) {
                let _ = writeln!(out, "{f}");
            }
        }
        Q::Map => {
            out.push_str(&graph().await.repo_map(40_000));
        }
    }
    Ok(out.trim_end().to_owned())
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
    Box::pin(run_loop(
        make_store(&pid, &state_dir).await?,
        &state_dir,
        work_dir,
        String::new(),
        max_cycles,
    ))
    .await
}

/// Serve many projects from a hub registry file — all roles, or the surface
/// selected by `COXAGENT_ROLE`. The registry is a JSON array of
/// `{ "id", "path" }` where `path` contains `state/` and `codebase/`.
///
/// One entry of the hub registry JSON array.
#[derive(serde::Deserialize)]
struct Entry {
    id: String,
    path: PathBuf,
}

/// # Errors
/// Returns an error when the registry can't be read or the port can't bind.
#[allow(clippy::too_many_lines)] // one linear wiring pass; splitting hurts readability
pub async fn run_hub(registry: &Path, mut port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // If the requested port is in use, scan upward for a free one so the hub
    // never fails to start — especially important when the Docker stack (which
    // uses port 4000 internally) and the desktop app share the same host.
    //
    // Moving is fine; moving QUIETLY is not. The desktop shell opens the
    // configured port, so a hub that slid to 4002 left the window pointed at
    // whatever else answered on 4000 — here, the agents' own build of this
    // project — and the app looked dead while everything was running.
    let requested = port;
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
    if port != requested {
        let squatter = port_holder(requested);
        tracing::warn!(
            "port {requested} is already taken{} — the hub moved to {port}. Anything pointed at              {requested} (the desktop window, bookmarks, the MCP endpoint) is talking to that              other process, not to this hub.",
            squatter.map_or(String::new(), |p| format!(" by {p}"))
        );
    }
    tracing::info!("hub binding to port {port}");
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
    let mut broken = Vec::new();
    for e in entries {
        let state_dir = e.path.join("state");
        let work_dir = e.path.join("codebase");
        match build_project(&e.id, &state_dir, work_dir, auth.as_ref()).await {
            Ok(p) => {
                tracing::info!("hub: registered project '{}'", p.id);
                projects.push(p);
            }
            // Loud, and carried into the dashboard: a project that fails to
            // load has no handle to serve, so without this record it would
            // simply be absent from /api/projects and the person looking for
            // it would have only the hub log to go on (COX-B043).
            Err(err) => {
                tracing::error!("hub: skipping '{}': {err}", e.id);
                broken.push(coxagent_presentation::BrokenProject {
                    id: e.id.clone(),
                    config_path: e.path.join("coxagent.json"),
                    error: err.to_string(),
                });
            }
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
            git: GitConfig::default(),
            workflow: WorkflowConfig::default(),
            architecture: Vec::new(),
            deploy: DeployConfig::default(),
            policy: PolicyConfig::default(),
            releases: ReleasesConfig::default(),
            coverage: coxagent_application::config::CoverageConfig::default(),
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
        broken,
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

    // Git-URL import: clone into the workspace first, then adopt it exactly
    // like a local brownfield import (remote detection pre-fills git config).
    let cloned: Option<std::path::PathBuf> = match req
        .git_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        Some(url) => {
            if !(url.starts_with("git@")
                || url.starts_with("https://")
                || url.starts_with("http://"))
            {
                return Err("git URL must start with git@, https:// or http://".to_owned());
            }
            let wd = proj_dir.join("codebase");
            let out = tokio::process::Command::new("git")
                .args(["clone", url])
                .arg(&wd)
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .map_err(|e| format!("spawn git clone: {e}"))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                return Err(format!(
                    "git clone failed: {}",
                    err.lines().last().unwrap_or("unknown error")
                ));
            }
            Some(wd)
        }
        None => None,
    };

    // Brownfield import: adopt the given codebase in place. Greenfield: scaffold
    // a fresh `codebase/` under the workspace.
    let work_dir = if let Some(path) = cloned.as_ref().or(req.existing.as_ref()) {
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
            EngineKind::Copilot => "auto".to_owned(),
            _ => continue,
        };
        push(d.kind, model);
    }
    out
}

fn build_engine(
    config: &Config,
    logs_dir: PathBuf,
    mcp: Option<&coxagent_infrastructure::engine::McpAccess>,
) -> Result<BuiltEngine, Box<dyn std::error::Error>> {
    if config.workflow.sandbox
        && matches!(
            coxagent_infrastructure::proc::sandbox_status(true),
            SandboxStatus::Unavailable(_) | SandboxStatus::Denied(_)
        )
    {
        tracing::warn!(
            "workflow.sandbox is on but no sandbox backend is available — agents run unsandboxed"
        );
    }
    let fallbacks = effective_fallbacks(config);
    let default = build_failover(
        &config.engine.default,
        &fallbacks,
        mcp,
        &config.engine.escalation,
        config.workflow.sandbox,
    )?;
    let mut per_role = std::collections::HashMap::new();
    for (role, choice) in &config.engine.per_role {
        match build_failover(
            choice,
            &fallbacks,
            mcp,
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
    // Provider catalogs drift: a saved `opencode` model can vanish upstream
    // (bizbrain dropped DeepSeek-V4-Pro and every run failed with an opaque
    // "Unexpected server error"). Compare what the config names against what
    // `opencode models` offers RIGHT NOW and say so at boot, while an operator
    // is still looking at the log — instead of the silent per-run failures.
    {
        use coxagent_application::config::EngineKind;
        let offered = coxagent_infrastructure::engine::discover_opencode_models();
        if !offered.is_empty() {
            let check = |label: &str, choice: &coxagent_application::config::EngineChoice| {
                if matches!(choice.engine, EngineKind::Opencode)
                    && !offered.iter().any(|m| m == &choice.model)
                {
                    tracing::warn!(
                        "{label} names opencode model '{}' which `opencode models` no longer offers — the provider may have removed it; its runs will fail until the config is updated",
                        choice.model
                    );
                }
            };
            check("default engine", &config.engine.default);
            for (role, choice) in &config.engine.per_role {
                check(&format!("per-role engine for {role:?}"), choice);
            }
        }
    }
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
    // One read settles both the `Config` and the deploy health-gate's host-port
    // probe, so a `deploy.host_port` this project cannot publish fails the gate
    // (COX-B035) or is healed (COX-B042) instead of drifting between the two.
    // A config that does not parse at all stops the run here (COX-B043).
    let LoadedConfig {
        config,
        host_port_probe,
    } = load_config_with_probe(state_dir)?;

    // Track config changes so engine can be reloaded at cycle boundaries without restart
    let mut config_hash = config_content_hash(state_dir);
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
    let (engine, meter) = build_engine(&config, logs_dir(state_dir), mcp.as_ref())?;
    let mut sleep = std::time::Duration::from_secs(config.workflow.sleep_seconds);

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
            // Which stored login to act as; empty = the CLI's active account.
            let account = config.git.account.clone();
            match config.git.provider.as_str() {
                "gitlab" => Some(Arc::new(coxagent_infrastructure::GlForge::new(
                    repo, base, wd,
                ))),
                "github" => Some(coxagent_infrastructure::github_forge(
                    repo, base, wd, account,
                )),
                _ => None,
            }
        } else {
            None
        };
    // Ship this operator's live logs to shared storage (MinIO) so the central
    // hub can show a remote operator's live agent log, not just local ones.
    spawn_log_uploader(state_dir, &work_dir);
    // Captured before the use case takes ownership: the capability probe needs
    // the same repo and git settings the agents will actually use.
    let (caps_config, caps_work_dir) = (config.clone(), work_dir.clone());
    let mut uc = RunCycleUseCase::new(Arc::clone(&store), engine, config, work_dir, context)
        .with_meter(meter)
        .with_deploy(std::sync::Arc::new(DockerComposeDeploy::new()))
        .with_host_port_probe(host_port_probe)
        .with_git(std::sync::Arc::new(
            coxagent_infrastructure::SystemGit::new(),
        ))
        .with_files(Some(Arc::new(
            coxagent_infrastructure::FsWorkspaceFiles::new(),
        )))
        .with_janitor(Some(Arc::new(coxagent_infrastructure::OsProcessJanitor)));
    if let Some(f) = forge {
        uc = uc.with_forge(f);
    }
    uc = uc.with_notifier(build_notifier(Arc::clone(&store), webhook));
    // Heartbeat the shared worker registry with the live role + ticket each phase,
    // so every dashboard shows this headless team's current agent.
    let hb_store = Arc::clone(&store);
    let hb_worker = worker.clone();

    // What THIS machine can actually launch. The hub serving the dashboard may
    // be a container with no agent CLI at all, so it cannot detect this for us.
    let hb_caps = local_caps(&caps_config, &caps_work_dir).await;
    uc.set_capabilities(hb_caps.clone());
    // Shared live phase + keepalive: a single engine call can run for tens of
    // minutes while the registry TTL is a few minutes, so without a mid-phase
    // refresh a busy operator would drop off the dashboard and look dead.
    let phase: Arc<Mutex<(String, String)>> =
        Arc::new(Mutex::new(("idle".to_owned(), String::new())));
    {
        let (s, w, phase) = (Arc::clone(&store), hb_worker.clone(), Arc::clone(&phase));
        let caps = hb_caps.clone();
        tokio::spawn(async move {
            // Announce presence at once, before the first sleep: an operator that
            // took 45s to appear is one the setup wizard has already declared
            // missing.
            {
                let now = coxagent_application::state::now_rfc3339();
                let _ = s.heartbeat_worker(&w, "idle", "", &caps, &now).await;
            }
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(45)).await;
                let (role, note) = phase
                    .lock()
                    .map_or_else(|_| ("idle".to_owned(), String::new()), |p| p.clone());
                // Beat even while idle. This operator is a machine with agent
                // CLIs on it, and the hub — a container that will never have
                // one — learns what the team can run only from this registry.
                // Skipping idle meant an operator waiting for its first Start
                // was invisible, so the dashboard swore no agent CLI existed
                // while one sat right here. It also drains queued jobs on its
                // own 15s poll regardless of Start, so advertising it does not
                // mislead the force-merge routing.
                let now = coxagent_application::state::now_rfc3339();
                let _ = s.heartbeat_worker(&w, &role, &note, &caps, &now).await;
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
        let caps = hb_caps.clone();
        tokio::spawn(async move {
            let now = coxagent_application::state::now_rfc3339();
            let _ = s.heartbeat_worker(&w, &role, &note, &caps, &now).await;
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

    // A human's force-merge queued by the hub is drained at the top of each
    // cycle (below), so the use case can stay owned + mut here — which is what
    // lets the loop hot-reload its engine when the config changes.
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

        // Force-merge jobs the hub queued: pick them up at the top of the cycle.
        // Boxed for the same reason run_cycle is: this future carries whole
        // engine-run paths. Polling it inline on the worker stack (after the
        // drain task was folded into the loop) overflowed the stack right
        // after "cycle loop started" — the heap is where it belongs.
        Box::pin(uc.drain_jobs()).await;

        // Hot-reload on a config change (a Settings edit) — rebuild the engine
        // and apply the new config NOW, no process restart. The old meter was
        // drained into state at the previous cycle's end, so the swap loses no
        // spend. If the rebuild fails, keep running on the previous engine.
        let new_hash = config_content_hash(state_dir);
        if new_hash != config_hash {
            config_hash = new_hash;
            let reloaded = match load_config_with_probe(state_dir) {
                Ok(l) => l.config,
                Err(e) => {
                    tracing::warn!("config changed but is invalid — keeping previous: {e}");
                    continue;
                }
            };
            match build_engine(&reloaded, logs_dir(state_dir), mcp.as_ref()) {
                Ok((engine, meter)) => {
                    sleep = std::time::Duration::from_secs(reloaded.workflow.sleep_seconds);
                    uc.reload(reloaded, engine, meter);
                    tracing::info!(
                        "config changed — engine reloaded and applied without a restart"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "config changed but engine rebuild failed; keeping previous: {e}"
                    );
                }
            }
        }

        // Boxed: a cycle future is ~17KB of agent-phase locals, and this loop
        // frame lives for the whole daemon's life.
        let report = Box::pin(uc.run_cycle(cycle)).await;
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
    worktree_at(work_dir, worker)
}

/// Materialize (or reuse) a git worktree for `slug` beside the repo and return
/// its path — or the original `work_dir` when this isn't a repo or the add
/// fails. This is what makes CONCURRENT runners real: two DEV agents sharing
/// one checkout could never both pass a green-suite DoD (each saw the other's
/// half-written changes — the overnight zero-throughput deadlock), so each
/// concurrency slot gets its own tree.
pub(crate) fn worktree_at(work_dir: PathBuf, slug: &str) -> PathBuf {
    let is_repo = std::process::Command::new("git")
        .arg("-C")
        .arg(&work_dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !is_repo {
        return work_dir;
    }
    let sanitized: String = slug
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Key the worktree to THIS repo, not just the caller's slug. Sanitizing
    // collapses distinct ids onto one name ("my.app" and "my-app"), and the
    // headless slug (operator@host) carries no project at all — either way two
    // projects sharing a parent dir would silently reuse each other's worktree
    // (an agent then edits the WRONG repo). A short hash of the canonical repo
    // path makes the name unique per repo; both callers flow through here.
    let repo_key = {
        use std::hash::{Hash as _, Hasher as _};
        let canon = std::fs::canonicalize(&work_dir).unwrap_or_else(|_| work_dir.clone());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        canon.hash(&mut h);
        format!(
            "{:08x}",
            u32::try_from(h.finish() & u64::from(u32::MAX)).unwrap_or(0)
        )
    };
    let slug = format!("{sanitized}-{repo_key}");
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

/// One-shot engine discovery + report for a machine that has agent CLIs on
/// PATH (the dashboard host itself may not). Detects local engines and sends
/// them through the hub's /store heartbeat so `/api/engines` populates without
/// waiting for a full runner cycle.
async fn run_probe(hub: &str, project: &str) -> Result<String, Box<dyn std::error::Error>> {
    use crate::builders::{detected_engines, operator_token_path};
    let caps = coxagent_application::ports::outbound::WorkerCaps {
        engines: detected_engines().into_iter().map(|(n, _)| n).collect(),
        ..Default::default()
    };
    if caps.engines.is_empty() {
        return Ok("No agent engines detected on PATH.\n".to_owned());
    }
    let token = match std::env::var("COXAGENT_REMOTE_TOKEN") {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_owned()),
        _ => match operator_token_path() {
            Some(path) => std::fs::read_to_string(path)
                .ok()
                .map(|text| text.trim().to_owned())
                .filter(|t| !t.is_empty()),
            None => None,
        },
    };
    let cfg = RestConfig {
        base_url: hub.trim_end_matches('/').to_owned(),
        project_id: project.to_owned(),
        token,
    };
    let store = RestStateStore::new(cfg)?;
    let worker = ["HOSTNAME", "HOST"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .unwrap_or_else(|| "probe".to_owned());
    let now = coxagent_application::state::now_rfc3339();
    store
        .heartbeat_worker(&worker, "probe", "", &caps, &now)
        .await?;
    Ok(format!(
        "Detected {} engine(s) and reported them to {hub}: {}",
        caps.engines.len(),
        caps.engines.join(", ")
    ))
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

        // Ephemeral port, not a fixed one: a fixed port collides with any
        // other hub already listening (a leftover dev hub, a second concurrent
        // `cargo test`), and the test then talks to a STRANGER's server whose
        // auth store never minted these tokens — which shows up as a baffling
        // 401 on the assertion below instead of a bind error.
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .expect("reserve a free port")
            .local_addr()
            .expect("local addr")
            .port();
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

    // These three checks share one hub instance (real TCP bind), so they run
    // as one #[tokio::test] rather than three that would each boot a hub.
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
/// Compute a hash of the config file content to detect changes at cycle boundary
fn config_content_hash(state_dir: &Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let root = state_dir.parent().unwrap_or(state_dir);
    let path = root.join("coxagent.json");

    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

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
    #[ignore = "hits the real Claude CLI and a live MCP endpoint; run it by hand"]
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
                label: None,
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
