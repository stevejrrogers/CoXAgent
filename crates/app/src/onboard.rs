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
