//! `coxagent` — the full CLI (hub / run / onboard / …). Service binaries
//! (cox-gateway, cox-realtime, cox-knowledge, cox-all) are thin role wrappers
//! over the same library.
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    coxagent_app::cli_main().await
}
