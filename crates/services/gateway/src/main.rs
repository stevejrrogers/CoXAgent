//! cox-gateway — gateway plane of CoXAgent. Thin 12-factor composition root:
//! the surface it may serve is enforced inside the server via COXAGENT_ROLE
//! (wrong endpoint → 503), config is env-only (COXAGENT_REGISTRY, COXAGENT_PORT
//! + the backing-store vars).
use std::path::PathBuf;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    coxagent_app::init_tracing();
    std::env::set_var("COXAGENT_ROLE", "gateway");
    // The control plane never spawns shells or engines.
    std::env::set_var("COXAGENT_NO_INLINE_EXEC", "1");
    let registry = std::env::var("COXAGENT_REGISTRY").map_or_else(
        |_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join("CoXAgent/registry.json")
        },
        PathBuf::from,
    );
    let port: u16 = std::env::var("COXAGENT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4000);
    coxagent_app::load_coordination(
        registry
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    );
    match coxagent_app::run_hub(&registry, port).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
