//! Greenfield onboarding ("cycle 0"): scaffold a workspace and stop at the
//! human gate. Drafts are written for the user to review before the loop runs.
//! Interactive PO/SA/BA/PD drafting arrives with the engine-driven wizard;
//! this is the deterministic scaffold it builds on.

use coxagent_application::config::Config;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Complexity, Priority, TicketType};
use std::path::Path;
use std::sync::Arc;

/// Scaffold `coxagent.json`, a `project_context.md` template, and seed the
/// FEAT-000 walking skeleton. Returns the message shown to the operator. Works
/// with any [`StateStorePort`] (JSON file or Postgres).
pub async fn greenfield<S: StateStorePort + 'static>(
    store: &Arc<S>,
    state_dir: &Path,
    name: &str,
    alias: Option<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut existing = store.load().await?;
    if !existing.tickets.is_empty() {
        return Err("workspace already has tickets; refusing to re-onboard".into());
    }

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
pub async fn brownfield<S: StateStorePort + 'static>(
    store: &Arc<S>,
    state_dir: &Path,
    name: &str,
    alias: Option<String>,
    codebase: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    if !codebase.exists() {
        return Err(format!("codebase path does not exist: {}", codebase.display()).into());
    }
    let mut state = store.load().await?;
    if !state.tickets.is_empty() {
        return Err("workspace already has tickets; refusing to re-onboard".into());
    }

    let alias = alias.map_or_else(
        || coxagent_application::state::derive_alias(name),
        |a| a.to_uppercase(),
    );
    state.alias = alias.clone();
    state.display_name = Some(name.to_owned());
    store.save(&state).await?;

    // Git is mandatory (branch-per-ticket, audit trail). Initialise + baseline
    // commit when the codebase is not yet a repository.
    let git_note = ensure_git_repo(codebase)?;

    // Comprehension pass: so the team adopts the project understanding it, not
    // blind. Index the code into a REPO_MAP the agents read first, and detect the
    // stack to (a) seed governance rules that match reality and (b) draft a real
    // project_context.md instead of an empty template.
    let repo_stats = build_repo_map(codebase);
    let (rules, stack_lines) = detect_stack(codebase);

    let root = state_dir.parent().unwrap_or(state_dir);
    let config_path = root.join("coxagent.json");
    if !config_path.exists() {
        let mut cfg = Config::default();
        cfg.architecture.clone_from(&rules);
        // Prefer opencode as default engine (supports any provider) if detected.
        let engine = coxagent_infrastructure::discover()
            .iter()
            .find(|d| d.kind == coxagent_application::config::EngineKind::Opencode)
            .map(|_| coxagent_application::config::EngineKind::Opencode)
            .unwrap_or(coxagent_application::config::EngineKind::Claude);
        cfg.engine.default.engine = engine;
        if engine == coxagent_application::config::EngineKind::Opencode {
            cfg.engine.default.model = "bizbrain/DeepSeek-V4-Pro".to_owned();
        }
        cfg.engine.auto_fallback = false;
        std::fs::write(&config_path, serde_json::to_string_pretty(&cfg)?)?;
    }
    let context_path = state_dir.join("project_context.md");
    if !context_path.exists() {
        std::fs::create_dir_all(state_dir)?;
        std::fs::write(
            &context_path,
            comprehension_context(name, &repo_stats, &stack_lines),
        )?;
    }

    // Deploy needs a compose file; seed a chore if the app is not dockerized.
    let adder = AddTicketUseCase::new(Arc::clone(store));
    let mut seeded = Vec::new();
    if !has_compose(codebase) {
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
            })
            .await?;
        seeded.push(format!("{id} (dockerize)"));
    }

    let seeded_line = if seeded.is_empty() {
        "Seeded: none (compose present)".to_owned()
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
    Ok(format!(
        "Adopted existing project '{name}' (alias {alias}) at {}.\n\
         {git_note}\n\
         Comprehension: {repo_stats}\n\
         {arch_line}\n\
         Wrote: {}\n       {}\n\
         {seeded_line}\n\n\
         REVIEW: skim {} (auto-drafted from the code) and the seeded backlog, then \
         run the team on `{}`.\n",
        codebase.display(),
        config_path.display(),
        context_path.display(),
        context_path.display(),
        codebase.display(),
    ))
}

/// Index the codebase into a graph + write `.coxagent/REPO_MAP.md` (the map the
/// agents read first to orient). Returns a one-line stat summary; best-effort.
fn build_repo_map(codebase: &Path) -> String {
    use coxagent_application::codegraph::CodeGraph;
    let g = CodeGraph::index(codebase);
    let _ = g.save(codebase);
    let _ = std::fs::create_dir_all(codebase.join(".coxagent"));
    let _ = std::fs::write(
        codebase.join(".coxagent").join("REPO_MAP.md"),
        g.repo_map(40_000),
    );
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
        return Ok("Git: existing repository (left as-is).".to_owned());
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
    Ok("Git: initialised repository + baseline commit.".to_owned())
}

/// Whether the codebase already has a docker-compose file.
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
