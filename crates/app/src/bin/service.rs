//! Shared main for the role binaries: cox-gateway / cox-realtime /
//! cox-knowledge / cox-all. The role comes from the binary NAME, surfaces are
//! enforced inside the server (`COXAGENT_ROLE`), and configuration is
//! env-only (COXAGENT_REGISTRY, COXAGENT_PORT) — these are 12-factor service
//! entrypoints, not interactive CLIs.
use std::path::PathBuf;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let exe = std::env::args().next().unwrap_or_default();
    let name = std::path::Path::new(&exe)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let role = match name.as_str() {
        "cox-gateway" => "gateway",
        "cox-realtime" => "realtime",
        "cox-knowledge" => "knowledge",
        _ => "all",
    };
    std::env::set_var("COXAGENT_ROLE", role);
    if role == "gateway" {
        // The control plane never spawns shells or engines.
        std::env::set_var("COXAGENT_NO_INLINE_EXEC", "1");
    }
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
            eprintln!("{name}: {e}");
            ExitCode::FAILURE
        }
    }
}
