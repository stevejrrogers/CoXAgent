//! CoXAgent composition root — the ONE place dependency injection happens.
//!
//! M0 wires the JSON state store into the `AddTicketUseCase` and prints a
//! report. This proves the layering compiles and runs end to end; real
//! subcommands (onboard, run, daemon, report, state) arrive in later milestones.

use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Complexity, Priority, TicketType};
use coxagent_infrastructure::JsonStateStore;
use coxagent_presentation::render_report;
use std::process::ExitCode;
use std::sync::Arc;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("coxagent: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<String, Box<dyn std::error::Error>> {
    let state_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "./state".to_owned());

    let store = Arc::new(JsonStateStore::new(state_dir)?);

    // Seed FEAT-000 walking skeleton if the backlog is empty (M0 smoke).
    let existing = store.load().await?;
    if existing.tickets.is_empty() {
        let uc = AddTicketUseCase::new(Arc::clone(&store));
        let id = uc
            .execute(AddTicketInput {
                ticket_type: TicketType::Feature,
                title: "Walking skeleton".to_owned(),
                description: "Hello-world service with /health".to_owned(),
                priority: Priority::High,
                complexity: Complexity::Small,
                has_ui: false,
            })
            .await?;
        eprintln!("seeded {id}");
    }

    let state = store.load().await?;
    Ok(render_report(&state))
}
