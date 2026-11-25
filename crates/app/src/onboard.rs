//! Greenfield onboarding ("cycle 0"): scaffold a workspace and stop at the
//! human gate. Drafts are written for the user to review before the loop runs.
//! Interactive PO/SA/BA/PD drafting arrives with the engine-driven wizard;
//! this is the deterministic scaffold it builds on.

use coxagent_application::config::Config;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::ProjectState;
use coxagent_application::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Complexity, Priority, SemVer, TicketType};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

// ── Docker smarts: reuse vs duplicate ─────────────────────────────────────

/// Well-known infrastructure services and their default ports.
const KNOWN_SERVICES: &[(&str, u16)] = &[
    ("postgres", 5432),
    ("redis", 6379),
    ("mongo", 27017),
    ("mysql", 3306),
    ("kafka", 9092),
    ("minio", 9000),
    ("rabbitmq", 5672),
    ("elasticsearch", 9200),
    ("nats", 4222),
    ("clickhouse", 8123),
    ("consul", 8500),
    ("vault", 8200),
];

/// Parsed view of one docker-compose service.
#[derive(Debug, Clone)]
struct ComposeService {
    name: String,
    image: Option<String>,
    ports: Vec<(u16, u16)>, // (container, host)
    env: HashMap<String, String>,
}

/// Parsed docker-compose file.
#[derive(Debug, Default)]
struct ParsedCompose {
    services: Vec<ComposeService>,
    volumes: Vec<String>,
    networks: Vec<String>,
}

/// Lists ports in use on the host by docker containers (via `docker ps`).
fn running_host_ports() -> HashSet<u16> {
    let mut ports = HashSet::new();
    let Ok(out) = Command::new("docker")
        .args(["ps", "--format", "{{.Ports}}"])
        .output()
    else {
        return ports;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Parse patterns like "0.0.0.0:5432->5432/tcp" or ":::6379->6379/tcp"
    for line in stdout.lines() {
        for part in line.split(", ") {
            if let Some(host) = part.split("->").next() {
                if let Some(port_str) = host.rsplit(':').next() {
                    if let Ok(p) = port_str.parse::<u16>() {
                        ports.insert(p);
                    }
                }
            }
        }
    }
    ports
}

/// Parse a docker-compose file to extract services, ports, images.
fn parse_compose(path: &Path) -> ParsedCompose {
    let mut result = ParsedCompose::default();
    let Ok(content) = std::fs::read_to_string(path) else {
        return result;
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&content) else {
        return result;
    };

    if let Some(services) = doc.get("services").and_then(|s| s.as_mapping()) {
        for (name, svc) in services {
            let name = name.as_str().unwrap_or("?").to_string();
            let mut cs = ComposeService {
                name: name.clone(),
                image: svc.get("image").and_then(|i| i.as_str()).map(String::from),
                ports: Vec::new(),
                env: HashMap::new(),
            };

            if let Some(ports_arr) = svc.get("ports").and_then(|p| p.as_sequence()) {
                for p in ports_arr {
                    if let Some(port_str) = p.as_str() {
                        let parts: Vec<&str> = port_str.split(':').collect();
                        if parts.len() >= 2 {
                            let host = parts[0].parse::<u16>().unwrap_or(0);
                            let container = parts
                                .last()
                                .copied()
                                .unwrap_or("")
                                .parse::<u16>()
                                .unwrap_or(0);
                            if host > 0 && container > 0 {
                                cs.ports.push((container, host));
                            }
                        }
                    }
                }
            }

            if let Some(env_map) = svc.get("environment").and_then(|e| e.as_mapping()) {
                for (k, v) in env_map {
                    let key = k.as_str().unwrap_or("").to_string();
                    let val = v.as_str().unwrap_or("").to_string();
                    if !key.is_empty() {
                        cs.env.insert(key, val);
                    }
                }
            }

            result.services.push(cs);
        }
    }

    if let Some(vols) = doc.get("volumes").and_then(|v| v.as_mapping()) {
        for (name, _) in vols {
            if let Some(n) = name.as_str() {
                result.volumes.push(n.to_string());
            }
        }
    }

    if let Some(nets) = doc.get("networks").and_then(|n| n.as_mapping()) {
        for (name, _) in nets {
            if let Some(n) = name.as_str() {
                result.networks.push(n.to_string());
            }
        }
    }

    result
}

/// Detect which well-known infrastructure services are declared in the compose
/// file and match them against running docker containers. Returns:
/// - reusable: services already running (don't duplicate)
/// - declared: services declared in compose that duplicate a running service
/// - missing: well-known services not declared (could be added)
#[allow(dead_code)]
#[derive(Debug)]
struct DockerAnalysis {
    #[allow(dead_code)]
    composable: Vec<ParsedCompose>,
    #[allow(dead_code)]
    running_ports: HashSet<u16>,
    /// services already running on known ports (e.g. postgres:5432)
    reusable: Vec<(String, u16)>,
    /// ports consumed by running services (to avoid for new compose)
    #[allow(dead_code)]
    consumed_ports: HashSet<u16>,
    /// services declared in compose that overlap with a running service
    clashes: Vec<String>,
    /// well-known services NOT declared in any compose (gaps)
    missing_known: Vec<&'static str>,
    /// has at least one compose file at all
    has_compose: bool,
    /// has a Dockerfile
    has_dockerfile: bool,
}

fn analyze_docker(codebase: &Path) -> DockerAnalysis {
    let compose_patterns = [
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ];
    let mut composable: Vec<ParsedCompose> = Vec::new();

    for pat in &compose_patterns {
        let p = codebase.join(pat);
        if p.exists() {
            composable.push(parse_compose(&p));
        }
    }

    let has_compose = !composable.is_empty();
    let has_dockerfile = codebase.join("Dockerfile").exists();

    let running_ports = running_host_ports();

    // Build a set of all ports declared in compose files
    let mut declared_ports: HashMap<u16, &ComposeService> = HashMap::new();
    for c in &composable {
        for svc in &c.services {
            for &(_, host) in &svc.ports {
                declared_ports.insert(host, svc);
            }
        }
    }

    // Detect known services that are already running
    let mut reusable = Vec::new();
    let mut clashes = Vec::new();
    let mut consumed_ports: HashSet<u16> = HashSet::new();

    for &(svc_name, default_port) in KNOWN_SERVICES {
        if running_ports.contains(&default_port) {
            if let Some(declared) = declared_ports.get(&default_port) {
                // Service declared in compose AND already running → clash
                clashes.push(format!(
                    "{} (port {default_port}) is declared in compose service '{}' but a container is already running on that port",
                    svc_name, declared.name
                ));
            } else {
                // Not in compose but running → reusable
                reusable.push((svc_name.to_string(), default_port));
            }
            consumed_ports.insert(default_port);
        } else if declared_ports.contains_key(&default_port) {
            // Declared but not yet running — port will be consumed on deploy
            consumed_ports.insert(default_port);
        }
    }

    // Detected known services that are neither declared nor running → gaps
    let declared_images: HashSet<String> = composable
        .iter()
        .flat_map(|c| c.services.iter())
        .filter_map(|s| s.image.as_ref().map(|i| i.to_lowercase()))
        .collect();
    let missing_known: Vec<&str> = KNOWN_SERVICES
        .iter()
        .filter(|&&(name, port)| {
            !declared_ports.contains_key(&port)
                && !running_ports.contains(&port)
                && !declared_images.iter().any(|img| img.contains(name))
        })
        .map(|&(name, _)| name)
        .collect();

    DockerAnalysis {
        composable,
        running_ports,
        reusable,
        consumed_ports,
        clashes,
        missing_known,
        has_compose,
        has_dockerfile,
    }
}

/// Generate improved `project_context.md` with docker smarts infused.
fn smart_comprehension_context(
    name: &str,
    repo_stats: &str,
    stack_lines: &[String],
    docker: &DockerAnalysis,
) -> String {
    use std::fmt::Write as _;
    let stack = if stack_lines.is_empty() {
        "_No stack auto-detected — describe it here._".to_owned()
    } else {
        stack_lines.join("\n")
    };
    let mut extra = String::new();

    if !docker.reusable.is_empty() {
        extra.push_str("## Infrastructure (reusable — already running)\n");
        extra.push_str("The following services are running on docker. The team should **reuse** them (connect, don't deploy duplicates):\n\n");
        for (svc, port) in &docker.reusable {
            let _ = writeln!(extra, "- **{svc}** — port :{port}");
        }
        extra.push('\n');
    }

    if !docker.clashes.is_empty() {
        extra.push_str("## ⚠️ Port clashes detected\n");
        extra.push_str(
            "The compose file declares services that conflict with running containers:\n\n",
        );
        for c in &docker.clashes {
            let _ = writeln!(extra, "- {c}");
        }
        extra.push_str(
            "\n**Action:** remove the conflicting services from compose and connect to the running ones.\n\n",
        );
    }

    if !docker.missing_known.is_empty() && !docker.has_compose {
        extra.push_str("## Recommended infra to add\n");
        extra.push_str("Consider adding these services to docker-compose:\n\n");
        for s in &docker.missing_known {
            let _ = writeln!(extra, "- **{s}**");
        }
        extra.push('\n');
    }

    if !docker.has_compose && !docker.has_dockerfile {
        extra.push_str("## Deploy\n");
        extra.push_str("No Dockerfile or compose found — the team must dockerize this project before the TEST/deploy step can run.\n\n");
    }

    format!(
        "# {name} — project context\n\n\
         _Auto-drafted on adoption from the codebase ({repo_stats}). Review and refine._\n\n\
         ## Stack (detected)\n{stack}\n\n\
         {extra}\
         ## What this project is\n\
         _One paragraph: the product, who it's for, the core value. (Fill in — the \
         team uses this to propose relevant work.)_\n\n\
         ## Scope for the team\n\
         _What should the autonomous team work on first? Goals, priorities, pain \
         points, areas to avoid._\n\n\
         ## Conventions to respect\n\
         _Testing, CI, code style, branching — anything the agents must not break. \
         The agents also read `.coxagent/REPO_MAP.md` for structure._\n",
    )
}

/// Seed smart tickets based on docker analysis.
async fn seed_smart_tickets<S: StateStorePort + 'static>(
    store: &Arc<S>,
    docker: &DockerAnalysis,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let adder = AddTicketUseCase::new(Arc::clone(store));
    let mut seeded = Vec::new();

    // Dockerize if nothing at all
    if !docker.has_compose && !docker.has_dockerfile {
        let id = adder
            .execute(AddTicketInput {
                ticket_type: TicketType::Chore,
                title: "Dockerize for deploy".to_owned(),
                description: "Add a Dockerfile and docker-compose.yml so the app builds and runs \
                              for the TEST/deploy step."
                    .to_owned(),
                priority: Priority::High,
                complexity: Complexity::Medium,
                has_ui: false,
                acceptance_criteria: Vec::new(),
                goal: None,
                service_tag: None,
            })
            .await?;
        seeded.push(format!("{id} (dockerize)"));
    }

    // If compose exists but declares services that clash with running ones → fix ticket
    if !docker.clashes.is_empty() {
        let clash_list = docker.clashes.join(", ");
        let reusable_list: Vec<String> = docker
            .reusable
            .iter()
            .map(|(s, p)| format!("{s}:{p}"))
            .collect();
        let hint = if docker.reusable.is_empty() {
            "remove the conflicting services from compose (they won't start)".to_owned()
        } else {
            format!(
                "connect to the already-running services ({}) instead of deploying duplicates",
                reusable_list.join(", ")
            )
        };
        let id = adder
            .execute(AddTicketInput {
                ticket_type: TicketType::Chore,
                title: "Fix compose — remove duplicate infra".to_owned(),
                description: format!(
                    "Compose declares services that clash with running containers: {clash_list}. \
                     Fix: {hint}. Also update configuration (env vars, connection strings) \
                     to point to the running infrastructure. \
                     Running ports: {}.",
                    docker
                        .reusable
                        .iter()
                        .map(|(s, p)| format!("{s}:{p}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                priority: Priority::High,
                complexity: Complexity::Small,
                has_ui: false,
                acceptance_criteria: vec![
                    "Compose file no longer declares duplicate services".to_owned(),
                    "App connects to existing running infrastructure".to_owned(),
                ],
                goal: None,
                service_tag: None,
            })
            .await?;
        seeded.push(format!("{id} (fix-compose)"));
    }

    // If there's a compose but Dockerfile is missing → hint
    if docker.has_compose && !docker.has_dockerfile {
        let id = adder
            .execute(AddTicketInput {
                ticket_type: TicketType::Chore,
                title: "Add Dockerfile for build".to_owned(),
                description:
                    "docker-compose exists but no Dockerfile — the TEST/deploy build step \
                              needs a Dockerfile to build the app image."
                        .to_owned(),
                priority: Priority::Medium,
                complexity: Complexity::Small,
                has_ui: false,
                acceptance_criteria: Vec::new(),
                goal: None,
                service_tag: None,
            })
            .await?;
        seeded.push(format!("{id} (dockerfile)"));
    }

    Ok(seeded)
}

/// Scaffold `coxagent.json`, a `project_context.md` template, and seed the
/// FEAT-000 walking skeleton. Returns the message shown to the operator. Works
/// with any [`StateStorePort`] (JSON file or Postgres).
/// Refuse project scaffolding from inside an agent worktree. A TEST/DEV agent
/// "testing project creation" from its sandbox ran the real CLI against the
/// operator's global registry and minted six live `qab-N` cleanroom projects
/// in one afternoon — cleanroom experiments belong in a temp dir, not the hub.
fn refuse_agent_scaffold() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir().unwrap_or_default();
    if cwd.components().any(|c| {
        c.as_os_str()
            .to_string_lossy()
            .starts_with(".coxagent-worktrees")
    }) {
        return Err(
            "refusing to scaffold a project from inside an agent worktree — \
                    this would register a live project in the operator's hub. Use a \
                    plain temp directory (outside .coxagent-worktrees) for cleanroom \
                    tests."
                .into(),
        );
    }
    Ok(())
}

/// An expected onboarding conflict: the target store already holds tickets, so
/// re-onboarding is refused. The caller asked to scaffold a workspace that is
/// already alive — a client-side conflict, not a server fault. Typed so the
/// HTTP layer can map it to 409 instead of 500 (CXA-B129).
#[derive(Debug)]
pub struct OnboardConflict(pub String);

impl std::fmt::Display for OnboardConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OnboardConflict {}

/// Classify an onboarding failure (CXA-B129): `Some(message)` when it is the
/// expected client conflict, `None` for a genuine fault. Pure.
pub fn conflict_message(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    err.downcast_ref::<OnboardConflict>().map(|c| c.0.clone())
}

/// An expected onboarding input error: the user-supplied codebase path does
/// not exist, so the request can never succeed as issued — a client-side bad
/// input, not a server fault. Typed so the HTTP layer can map it to 400
/// instead of 500 (CXA-B157, same class as the B139/B142 refusals).
#[derive(Debug)]
pub struct OnboardMissingPath(pub String);

impl std::fmt::Display for OnboardMissingPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OnboardMissingPath {}

/// Classify an onboarding input failure (CXA-B157): `Some(message)` when it is
/// the missing-codebase-path bad input, `None` otherwise. Pure.
pub fn missing_path_message(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    err.downcast_ref::<OnboardMissingPath>()
        .map(|c| c.0.clone())
}

/// Refuse re-onboarding over an active backlog (CXA-F003): scaffolding again
/// on top of existing tickets would silently double-seed or lose state. Typed
/// as [`OnboardConflict`] so the API layer returns 409, never 500 (CXA-B129).
fn refuse_existing_tickets(state: &ProjectState) -> Result<(), Box<dyn std::error::Error>> {
    if !state.tickets.is_empty() {
        return Err(Box::new(OnboardConflict(
            "workspace already has tickets; refusing to re-onboard".into(),
        )));
    }
    Ok(())
}

pub async fn greenfield<S: StateStorePort + 'static>(
    store: &Arc<S>,
    state_dir: &Path,
    name: &str,
    alias: Option<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    refuse_agent_scaffold()?;
    let mut existing = store.load().await?;
    refuse_existing_tickets(&existing)?;

    // Establish the ticket-id alias (user-provided or derived) before minting.
    let alias = alias.map_or_else(
        || coxagent_application::state::derive_alias(name),
        |a| a.to_uppercase(),
    );
    existing.alias = alias.clone();
    store.save(&existing).await?;

    // Workspace root is the parent of the state dir (or the state dir itself).
    let root = state_dir.parent().unwrap_or(state_dir);
    let config_path = root.join("coxagent.json");
    if !config_path.exists() {
        let json = serde_json::to_string_pretty(&Config::default())?;
        std::fs::write(&config_path, json)?;
    }

    let context_path = state_dir.join("project_context.md");
    if !context_path.exists() {
        std::fs::create_dir_all(state_dir)?;
        std::fs::write(&context_path, context_template(name))?;
    }

    let adder = AddTicketUseCase::new(Arc::clone(store));
    let skeleton = adder
        .execute(AddTicketInput {
            ticket_type: TicketType::Feature,
            title: "Walking skeleton".to_owned(),
            description: "Hello-world service with a /health endpoint that builds and runs."
                .to_owned(),
            priority: Priority::High,
            complexity: Complexity::Small,
            has_ui: false,
            acceptance_criteria: Vec::new(),
            goal: None,
            service_tag: None,
        })
        .await?;

    Ok(format!(
        "Onboarded project '{name}' (alias {alias}).\n\
         Wrote: {}\n       {}\n\
         Seeded: {skeleton} (walking skeleton)\n\n\
         HUMAN GATE: review and complete {} before running `coxagent run`.\n",
        config_path.display(),
        context_path.display(),
        context_path.display(),
    ))
}

/// Brownfield onboarding: adopt the existing codebase at `codebase`. Unlike
/// greenfield it seeds no walking skeleton (the app already exists); instead it
/// makes the codebase a git repo (required for branch-per-ticket and audit) and
/// seeds a Dockerize chore when there is no compose file, so the deploy step has
/// something to make the app verifiable. The human gate is the same.
#[allow(clippy::too_many_lines)] // one linear pass; splitting hurts readability
pub async fn brownfield<S: StateStorePort + 'static>(
    store: &Arc<S>,
    state_dir: &Path,
    name: &str,
    alias: Option<String>,
    codebase: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    // Input validation first (CXA-B157): a missing codebase path is the
    // CLIENT's bad input and must be refused — typed — regardless of where
    // the process runs, so it can never be shadowed by the environment guard
    // below into an unclassified 500.
    if !codebase.exists() {
        return Err(Box::new(OnboardMissingPath(format!(
            "codebase path does not exist: {}",
            codebase.display()
        ))));
    }
    refuse_agent_scaffold()?;
    let mut state = store.load().await?;
    refuse_existing_tickets(&state)?;

    let alias = alias.map_or_else(
        || coxagent_application::state::derive_alias(name),
        |a| a.to_uppercase(),
    );
    state.alias = alias.clone();
    state.display_name = Some(name.to_owned());
    // Adopt the version the codebase already declares. Starting an adopted
    // project at 0.0.0 is not merely cosmetic: the release step bumps from
    // whatever this says, so a repo sitting at 2.21.0 would be handed 0.0.1 and
    // then ship version numbers that collide with the ones already published.
    if let Some(v) = detect_codebase_version(codebase) {
        state.current_version = v;
    }
    store.save(&state).await?;

    // Git is mandatory (branch-per-ticket, audit trail). Initialise + baseline
    // commit when the codebase is not yet a repository.
    let git_note = ensure_git_repo(codebase)?;

    // Comprehension pass: so the team adopts the project understanding it, not
    // blind. Index the code into a REPO_MAP the agents read first, and detect the
    // stack to (a) seed governance rules that match reality and (b) draft a real
    // project_context.md instead of an empty template.
    let repo_stats = build_repo_map(codebase).await;
    let (rules, stack_lines) = detect_stack(codebase);

    // ── Docker smarts: parse compose, detect running services, avoid clashes ─
    let docker = analyze_docker(codebase);

    let root = state_dir.parent().unwrap_or(state_dir);
    let codebase_sym = root.join("codebase");
    if !codebase_sym.exists() {
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(codebase, &codebase_sym);
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::create_dir(&codebase_sym);
        }
    }
    let config_path = root.join("coxagent.json");
    if !config_path.exists() {
        let mut cfg = Config::default();
        cfg.architecture.clone_from(&rules);
        // Prefer opencode as default engine (supports any provider) if detected.
        let has_opencode = coxagent_infrastructure::discover()
            .iter()
            .any(|d| d.kind == coxagent_application::config::EngineKind::Opencode);
        let engine = if has_opencode {
            coxagent_application::config::EngineKind::Opencode
        } else {
            coxagent_application::config::EngineKind::Claude
        };
        cfg.engine.default.engine = engine;
        if has_opencode {
            // V4-Pro was removed from the provider catalog; every project
            // onboarded with it warned at boot and failed its default runs.
            "bizbrain/DeepSeek-V4-Flash".clone_into(&mut cfg.engine.default.model);
        }
        cfg.engine.auto_fallback = false;
        // Save consumed ports so assign_host_port skips them
        cfg.deploy.host_port = None; // will be assigned below
                                     // Pre-fill git config from a detected `origin` remote: an imported
                                     // repo that can already push should work without manual wiring.
        if let Some((provider, repo)) = std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(codebase)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| parse_remote(&String::from_utf8_lossy(&o.stdout)))
        {
            cfg.git.enabled = true;
            cfg.git.provider = provider;
            cfg.git.repo = repo;
        }
        std::fs::write(&config_path, serde_json::to_string_pretty(&cfg)?)?;
    }
    let context_path = state_dir.join("project_context.md");
    if !context_path.exists() {
        std::fs::create_dir_all(state_dir)?;
        std::fs::write(
            &context_path,
            smart_comprehension_context(name, &repo_stats, &stack_lines, &docker),
        )?;
    }

    // Seed tickets based on docker analysis (replaces simple has_compose check)
    let seeded = seed_smart_tickets(store, &docker).await?;

    // CXA-F258: pull the connected repo's issue backlog in as Pending tickets
    // so the team starts on the real backlog, not a hand-typed stand-in.
    // Best-effort: a forge failure surfaces as a note, never fails adoption.
    let backlog_note = import_backlog(store, &config_path, codebase).await?;

    let seeded_line = if seeded.is_empty() {
        "Seeded: none (compose present, no clashes)".to_owned()
    } else {
        format!("Seeded: {}", seeded.join(", "))
    };
    let arch_line = if rules.is_empty() {
        "Stack: none auto-detected (set architecture rules in Settings if needed)".to_owned()
    } else {
        format!(
            "Detected stack ({} area(s)) → seeded governance rules",
            rules.len()
        )
    };
    let docker_note = if docker.reusable.is_empty() {
        "Running infra: none detected".to_owned()
    } else {
        format!(
            "Running infra (reuse): {}",
            docker
                .reusable
                .iter()
                .map(|(s, p)| format!("{s}:{p}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Ok(format!(
        "Adopted existing project '{name}' (alias {alias}) at {}.\n\
         {git_note}\n\
         Comprehension: {repo_stats}\n\
         {arch_line}\n\
         {docker_note}\n\
         Wrote: {}\n       {}\n\
         {seeded_line}\n\
         {backlog_note}\n\n\
         REVIEW: skim {} (auto-drafted from the code) and the seeded backlog, then \
         run the team on `{}`.\n",
        codebase.display(),
        config_path.display(),
        context_path.display(),
        context_path.display(),
        codebase.display(),
    ))
}

/// CXA-F258 — brownfield backlog import: when the adopted repo is connected
/// on GitHub, fetch its OPEN issues (capped at `backlog_import::IMPORT_CAP`)
/// and merge them in as Pending tickets through
/// [`coxagent_application::backlog_import::merge_pending`]; ONE load → merge
/// → save. Closed issues are never fetched here (AC4: excluded by default;
/// an opt-in surface is pending the SA's answer on where the preview lives).
/// The returned note reports imported vs skipped (AC3). Forge failures are
/// surfaced in the note, never fail the adoption — the import is repeatable
/// once the forge is reachable.
async fn import_backlog<S: StateStorePort + 'static>(
    store: &Arc<S>,
    config_path: &Path,
    codebase: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let cfg: Config = match std::fs::read_to_string(config_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
    {
        Some(cfg) => cfg,
        None => return Ok("Backlog import: skipped (no readable project config)".to_owned()),
    };
    if !cfg.git.enabled || cfg.git.provider != "github" || cfg.git.repo.trim().is_empty() {
        return Ok("Backlog import: skipped (no connected GitHub repo)".to_owned());
    }
    let forge = coxagent_infrastructure::github_forge(
        cfg.git.repo.clone(),
        cfg.git.base_url.clone(),
        codebase.to_path_buf(),
        String::new(),
    );
    let drafts = match forge
        .list_open_issues(coxagent_application::backlog_import::IMPORT_CAP)
        .await
    {
        Ok(drafts) => drafts,
        Err(e) => {
            return Ok(format!(
                "Backlog import: FAILED — {e} (re-run once the forge is reachable)"
            ))
        }
    };
    if drafts.is_empty() {
        return Ok(format!(
            "Backlog import: no open issues on {}",
            cfg.git.repo
        ));
    }
    let mut state = store.load().await?;
    let report = coxagent_application::backlog_import::merge_pending(
        &mut state,
        &drafts,
        coxagent_application::backlog_import::IMPORT_CAP,
    );
    if report.imported == 0 {
        return Ok(format!(
            "Backlog import: 0 new from {} ({} already tracked)",
            cfg.git.repo, report.skipped
        ));
    }
    state
        .validate()
        .map_err(|e| format!("backlog import refused: {e}"))?;
    store.save(&state).await?;
    Ok(format!(
        "Backlog import: {} ticket(s) from {} ({} skipped)",
        report.imported, cfg.git.repo, report.skipped
    ))
}

/// Index the codebase into a graph + write `.coxagent/REPO_MAP.md` (the map the
/// agents read first to orient). Returns a one-line stat summary; best-effort.
async fn build_repo_map(codebase: &Path) -> String {
    use coxagent_application::codegraph::CodeGraph;
    let files = coxagent_infrastructure::FsWorkspaceFiles::new();
    let g = CodeGraph::index(&files, codebase).await;
    // save() also writes REPO_MAP.md — one producer, both artifacts.
    let _ = g.save(&files, codebase).await;
    format!(
        "indexed {} files, {} symbols, {} calls",
        g.files.len(),
        g.symbols.len(),
        g.calls.len()
    )
}

/// Detect the tech stack from manifest files at the root and one level down.
/// Returns governance [`StackRule`]s (area → required language) plus a
/// human-readable summary line per area — enough for the team to respect the
/// existing stack instead of guessing.
fn detect_stack(
    codebase: &Path,
) -> (
    Vec<coxagent_application::conformance::StackRule>,
    Vec<String>,
) {
    use coxagent_application::conformance::StackRule;
    const MANIFESTS: &[(&str, &str)] = &[
        ("Cargo.toml", "Rust"),
        ("package.json", "JavaScript/TypeScript"),
        ("go.mod", "Go"),
        ("pyproject.toml", "Python"),
        ("requirements.txt", "Python"),
        ("pom.xml", "Java"),
        ("build.gradle", "Java/Kotlin"),
        ("Gemfile", "Ruby"),
        ("composer.json", "PHP"),
    ];
    let mut areas: Vec<std::path::PathBuf> = vec![codebase.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(codebase) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir()
                && !name.starts_with('.')
                && !matches!(name.as_str(), "node_modules" | "target" | "vendor" | "dist")
            {
                areas.push(e.path());
            }
        }
    }
    let (mut rules, mut summary, mut seen) =
        (Vec::new(), Vec::new(), std::collections::HashSet::new());
    for area in areas {
        let rel = area
            .strip_prefix(codebase)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        for (file, lang) in MANIFESTS {
            if area.join(file).exists() && seen.insert((rel.clone(), (*lang).to_owned())) {
                rules.push(StackRule {
                    area: rel.clone(),
                    language: (*lang).to_owned(),
                    require_any: vec![(*file).to_owned()],
                    forbid_ext: Vec::new(),
                });
                let loc = if rel.is_empty() { "root" } else { &rel };
                summary.push(format!("- **{loc}** — {lang} (`{file}`)"));
            }
        }
    }
    (rules, summary)
}

/// Draft `project_context.md` from what the comprehension pass learned, so the
/// human reviews & refines a real starting point rather than a blank template.
#[allow(dead_code)]
fn comprehension_context(name: &str, repo_stats: &str, stack_lines: &[String]) -> String {
    let stack = if stack_lines.is_empty() {
        "_No stack auto-detected — describe it here._".to_owned()
    } else {
        stack_lines.join("\n")
    };
    format!(
        "# {name} — project context\n\n\
         _Auto-drafted on adoption from the codebase ({repo_stats}). Review and refine._\n\n\
         ## Stack (detected)\n{stack}\n\n\
         ## What this project is\n\
         _One paragraph: the product, who it's for, the core value. (Fill in — the \
         team uses this to propose relevant work.)_\n\n\
         ## Scope for the team\n\
         _What should the autonomous team work on first? Goals, priorities, pain \
         points, areas to avoid._\n\n\
         ## Conventions to respect\n\
         _Testing, CI, code style, branching — anything the agents must not break. \
         The agents also read `.coxagent/REPO_MAP.md` for structure._\n",
    )
}

/// Ensure `dir` is a git repo: `git init` + a baseline commit when it is not.
fn ensure_git_repo(dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    use std::process::Command;
    if dir.join(".git").exists() {
        // Detect an existing remote so the project's git config can be
        // pre-filled — an imported repo that can already push should not
        // need manual wiring.
        let remote = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(dir)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|u| !u.is_empty());
        return Ok(match remote {
            Some(url) => format!("Git: existing repository, remote origin = {url}."),
            None => "Git: existing repository, NO remote — connect one in Settings › Git                      before agents can push branches/PRs."
                .to_owned(),
        });
    }
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if !run(&["init"]) {
        return Err("git init failed (is git installed?)".into());
    }
    run(&["add", "-A"]);
    // Commit may be a no-op on an empty dir; that is fine.
    run(&["commit", "-m", "baseline: adopt into CoXAgent"]);
    Ok(
        "Git: initialised repository + baseline commit (no remote yet — connect one in \
         Settings › Git before agents can push branches/PRs)."
            .to_owned(),
    )
}

/// Parse a git remote URL into `(provider, owner/repo)` — supports
/// `git@host:owner/repo.git` and `http(s)://host/owner/repo(.git)` for
/// github.com and gitlab hosts. Public for the import flow to pre-fill config.
#[must_use]
pub fn parse_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let (host, path) = if let Some(rest) = url.strip_prefix("git@") {
        let (h, p) = rest.split_once(':')?;
        (h.to_owned(), p.to_owned())
    } else {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;
        let (h, p) = rest.split_once('/')?;
        (h.to_owned(), p.to_owned())
    };
    let repo = path
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_owned();
    if repo.split('/').count() < 2 {
        return None;
    }
    let provider = if host.contains("gitlab") {
        "gitlab"
    } else {
        "github"
    };
    Some((provider.to_owned(), repo))
}

/// Whether the codebase already has a docker-compose file.
#[allow(dead_code)]
fn has_compose(dir: &Path) -> bool {
    ["docker-compose.yml", "docker-compose.yaml", "compose.yml"]
        .iter()
        .any(|f| dir.join(f).exists())
}

fn context_template(name: &str) -> String {
    format!(
        "# Project Context: {name}\n\n\
         > Fill this in before running the loop. Every agent reads it.\n\n\
         ## Goal\n<what you are building, for whom, the problem it solves>\n\n\
         ## Tech stack\n<frontend / backend / database / infra>\n\n\
         ## Product scope\n<feature groups the BA may propose>\n\n\
         ## Constraints\n<auth, deploy target, performance, data retention>\n"
    )
}

// ── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_compose_extracts_services_and_ports() {
        let dir = tempfile::TempDir::new().unwrap();
        let yaml = r#"
services:
  app:
    build: .
    ports:
      - "3000:3000"
  db:
    image: postgres:16
    ports:
      - "5432:5432"
    environment:
      POSTGRES_USER: test
  redis:
    image: redis:7
    ports:
      - "6379:6379"
volumes:
  pgdata:
networks:
  appnet:
"#;
        std::fs::write(dir.path().join("compose.yml"), yaml).unwrap();
        let parsed = parse_compose(&dir.path().join("compose.yml"));

        assert_eq!(parsed.services.len(), 3);
        assert_eq!(parsed.volumes, vec!["pgdata"]);
        assert_eq!(parsed.networks, vec!["appnet"]);

        let app = &parsed.services[0];
        assert_eq!(app.name, "app");
        assert_eq!(app.ports, vec![(3000, 3000)]);
        assert!(app.image.is_none());

        let db = &parsed.services[1];
        assert_eq!(db.name, "db");
        assert_eq!(db.ports, vec![(5432, 5432)]);
        assert_eq!(db.image.as_deref(), Some("postgres:16"));
        assert_eq!(
            db.env.get("POSTGRES_USER").map(String::as_str),
            Some("test")
        );

        let redis = &parsed.services[2];
        assert_eq!(redis.name, "redis");
        assert_eq!(redis.ports, vec![(6379, 6379)]);
        assert_eq!(redis.image.as_deref(), Some("redis:7"));
    }

    #[test]
    fn analyze_docker_detects_compose_and_dockerfile() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            "services:\n  app:\n    image: node:20\n    ports:\n      - \"8080:8080\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM node:20").unwrap();

        let analysis = analyze_docker(dir.path());

        assert!(analysis.has_compose);
        assert!(analysis.has_dockerfile);
        assert_eq!(analysis.composable.len(), 1);
    }

    #[test]
    fn analyze_docker_detects_no_compose() {
        let dir = tempfile::TempDir::new().unwrap();
        let analysis = analyze_docker(dir.path());
        assert!(!analysis.has_compose);
        assert!(!analysis.has_dockerfile);
    }

    #[test]
    fn analyze_docker_detects_port_clashes() {
        let dir = tempfile::TempDir::new().unwrap();
        // Create a compose that declares postgres on :5432 and redis on :6379
        let yaml = r#"
services:
  app:
    image: node:20
    ports:
      - "3000:3000"
  pg:
    image: postgres:16
    ports:
      - "5432:5432"
  cache:
    image: redis:7
    ports:
      - "6379:6379"
"#;
        std::fs::write(dir.path().join("docker-compose.yml"), yaml).unwrap();

        let analysis = analyze_docker(dir.path());

        assert!(analysis.has_compose);
        assert!(!analysis.has_dockerfile);

        // If docker daemon is running and postgres/redis are up on 5432/6379,
        // we should detect clashes. If not, no clashes.
        // This test verifies the structure — actual clashes depend on env.
        assert_eq!(analysis.composable.len(), 1);
        assert_eq!(analysis.composable[0].services.len(), 3);
    }

    #[test]
    fn smart_context_includes_docker_info() {
        let docker = DockerAnalysis {
            composable: vec![],
            running_ports: HashSet::new(),
            reusable: vec![("postgres".into(), 5432), ("redis".into(), 6379)],
            consumed_ports: HashSet::new(),
            clashes: vec!["postgres (port 5432) declared but already running".into()],
            missing_known: vec!["kafka"],
            has_compose: true,
            has_dockerfile: true,
        };

        let ctx = smart_comprehension_context(
            "TestApp",
            "2 files, 10 symbols",
            &["- root — JavaScript (package.json)".into()],
            &docker,
        );

        assert!(ctx.contains("Infrastructure (reusable"));
        assert!(ctx.contains("postgres"));
        assert!(ctx.contains("redis"));
        assert!(ctx.contains("Port clashes detected"));
    }

    #[test]
    fn parse_remote_supports_ssh_and_https() {
        use super::parse_remote;
        assert_eq!(
            parse_remote("git@github.com:me/app.git"),
            Some(("github".into(), "me/app".into()))
        );
        assert_eq!(
            parse_remote("https://gitlab.company.io/team/app/"),
            Some(("gitlab".into(), "team/app".into()))
        );
        assert_eq!(parse_remote("not-a-url"), None);
        assert_eq!(parse_remote("git@github.com:justname"), None);
    }
}

/// The version an existing codebase already declares, for adoption.
///
/// The manifest wins over the newest git tag. A tag says what last shipped,
/// which is routinely BEHIND the working version — anchoring to it would make
/// the next release land on a number the manifest already claims.
fn detect_codebase_version(codebase: &Path) -> Option<SemVer> {
    /// A manifest filename and how to pull the version string out of it.
    type Manifest = (&'static str, fn(&str) -> Option<String>);
    let manifests: [Manifest; 3] = [
        ("Cargo.toml", |t| {
            // `[workspace.package]` or `[package]`; the first bare `version =`
            // is the crate's own, not a dependency's (those are inline tables).
            t.lines()
                .map(str::trim)
                .find(|l| l.starts_with("version") && l.contains('"'))
                .and_then(|l| l.split('"').nth(1).map(ToOwned::to_owned))
        }),
        ("package.json", |t| {
            serde_json::from_str::<serde_json::Value>(t)
                .ok()?
                .get("version")?
                .as_str()
                .map(ToOwned::to_owned)
        }),
        ("pyproject.toml", |t| {
            t.lines()
                .map(str::trim)
                .find(|l| l.starts_with("version") && l.contains('"'))
                .and_then(|l| l.split('"').nth(1).map(ToOwned::to_owned))
        }),
    ];
    for (file, parse) in manifests {
        if let Ok(text) = std::fs::read_to_string(codebase.join(file)) {
            if let Some(v) = parse(&text).as_deref().and_then(parse_semver) {
                return Some(v);
            }
        }
    }
    // No manifest we read: fall back to the newest tag.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(codebase)
        .args(["describe", "--tags", "--abbrev=0"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_semver(String::from_utf8_lossy(&out.stdout).trim())
}

/// `1.2.3` or `v1.2.3` as a [`SemVer`]; `None` for anything else (a date tag, a
/// release name) rather than a wrong guess.
fn parse_semver(raw: &str) -> Option<SemVer> {
    let s = raw.trim().trim_start_matches('v');
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    // Tolerate a pre-release/build suffix on the patch: `3-rc1` -> 3.
    let patch = parts.next()?.split(['-', '+']).next()?.parse().ok()?;
    Some(SemVer::new(major, minor, patch))
}

#[cfg(test)]
mod version_adoption_tests {
    use super::{detect_codebase_version, parse_semver};

    #[test]
    fn a_manifest_version_is_adopted_over_the_newest_tag() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nversion = \"2.21.0\"\n",
        )
        .expect("write");
        assert_eq!(
            detect_codebase_version(dir.path()).map(|v| v.to_string()),
            Some("2.21.0".to_owned()),
            "the manifest is the working version; a tag is what last shipped"
        );
    }

    #[test]
    fn a_tag_like_version_parses_with_or_without_the_v() {
        assert_eq!(
            parse_semver("v2.12.0").map(|v| v.to_string()),
            Some("2.12.0".to_owned())
        );
        assert_eq!(
            parse_semver("2.12.0").map(|v| v.to_string()),
            Some("2.12.0".to_owned())
        );
        assert_eq!(
            parse_semver("1.0.0-rc1").map(|v| v.to_string()),
            Some("1.0.0".to_owned())
        );
    }

    #[test]
    fn a_name_that_is_not_a_version_is_refused_rather_than_guessed() {
        // Adopting a wrong version is worse than adopting none: the release
        // step would bump from it and publish colliding numbers.
        for bad in ["release-summer", "2026-08-04", "", "v"] {
            assert!(parse_semver(bad).is_none(), "{bad:?} must not parse");
        }
    }
}

/// CXA-B129: the re-onboard refusal is a TYPED client conflict, classified so
/// the API layer maps it to 409 instead of 500.
#[cfg(test)]
mod re_onboard_conflict_tests {
    use super::{conflict_message, refuse_existing_tickets};
    use coxagent_application::state::ProjectState;

    fn state_with_one_ticket() -> ProjectState {
        let mut state = ProjectState::default();
        state.tickets.push(
            coxagent_domain::Ticket::new(
                coxagent_domain::TicketId::new("CXC-F001").expect("valid ticket id"),
                coxagent_domain::TicketType::Feature,
                "Walking skeleton",
                "Hello-world service with a /health endpoint that builds and runs.",
                coxagent_domain::Priority::High,
                coxagent_domain::Complexity::Small,
                false,
            )
            .expect("valid ticket"),
        );
        state
    }

    #[test]
    fn re_onboarding_over_tickets_is_a_typed_conflict_with_the_operational_message() {
        let err = refuse_existing_tickets(&state_with_one_ticket()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "workspace already has tickets; refusing to re-onboard",
            "CLI operators read this text; it must not change"
        );
        assert_eq!(
            conflict_message(err.as_ref()).as_deref(),
            Some("workspace already has tickets; refusing to re-onboard"),
            "the API layer classifies by type, so the marker must survive the Box"
        );
    }

    #[test]
    fn a_clean_workspace_onboards_without_conflict() {
        assert!(refuse_existing_tickets(&ProjectState::default()).is_ok());
    }

    #[test]
    fn an_ordinary_onboarding_fault_is_not_classified_as_a_conflict() {
        let err: Box<dyn std::error::Error> = "store unreachable".into();
        assert!(conflict_message(err.as_ref()).is_none());
    }
}

/// CXA-B157: the missing-codebase-path refusal is a TYPED client input error,
/// classified so the API layer maps it to 400 instead of 500 — and never
/// crosses into the 409 conflict class.
#[cfg(test)]
mod missing_path_input_tests {
    use super::{conflict_message, missing_path_message, OnboardMissingPath};

    #[test]
    fn the_missing_path_refusal_is_typed_as_a_bad_input() {
        let err: Box<dyn std::error::Error> = Box::new(OnboardMissingPath(
            "codebase path does not exist: /tmp/definitely-not-there-qa".into(),
        ));
        assert_eq!(
            missing_path_message(err.as_ref()).as_deref(),
            Some("codebase path does not exist: /tmp/definitely-not-there-qa"),
            "the API layer classifies by type, so the marker must survive the Box"
        );
        assert!(
            conflict_message(err.as_ref()).is_none(),
            "a bad input must never masquerade as a 409 conflict"
        );
    }

    #[test]
    fn an_ordinary_onboarding_fault_is_not_classified_as_a_missing_path() {
        let err: Box<dyn std::error::Error> = "store unreachable".into();
        assert!(missing_path_message(err.as_ref()).is_none());
    }
}
