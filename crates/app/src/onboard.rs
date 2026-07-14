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
    store.save(&state).await?;

    let root = state_dir.parent().unwrap_or(state_dir);
    let config_path = root.join("coxagent.json");
    if !config_path.exists() {
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&Config::default())?,
        )?;
    }
    let context_path = state_dir.join("project_context.md");
    if !context_path.exists() {
        std::fs::create_dir_all(state_dir)?;
        std::fs::write(&context_path, context_template(name))?;
    }

    // Git is mandatory (branch-per-ticket, audit trail). Initialise + baseline
    // commit when the codebase is not yet a repository.
    let git_note = ensure_git_repo(codebase)?;

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
    Ok(format!(
        "Adopted existing project '{name}' (alias {alias}) at {}.\n\
         {git_note}\n\
         Wrote: {}\n       {}\n\
         {seeded_line}\n\n\
         HUMAN GATE: complete {} (describe the existing stack & scope) before \
         running `coxagent run --work-dir {}`.\n",
        codebase.display(),
        config_path.display(),
        context_path.display(),
        context_path.display(),
        codebase.display(),
    ))
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
