//! cox-runner — the EXECUTION plane: runs one project's agent cycle loop
//! (engines, git, docker, queued jobs like force-merge). The only service that
//! ever spawns shells — isolate and resource-cap it in production.
//! Env: COXAGENT_STATE_DIR (required), COXAGENT_WORK_DIR (required),
//! COXAGENT_OPERATOR (identity), COXAGENT_MAX_CYCLES (optional).
use std::path::PathBuf;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let Some(state_dir) = std::env::var("COXAGENT_STATE_DIR").ok().map(PathBuf::from) else {
        eprintln!("cox-runner: COXAGENT_STATE_DIR is required");
        return ExitCode::FAILURE;
    };
    let Some(work_dir) = std::env::var("COXAGENT_WORK_DIR").ok().map(PathBuf::from) else {
        eprintln!("cox-runner: COXAGENT_WORK_DIR is required");
        return ExitCode::FAILURE;
    };
    let max_cycles = std::env::var("COXAGENT_MAX_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok());
    match coxagent_app::operator_main(state_dir, work_dir, max_cycles).await {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cox-runner: {e}");
            ExitCode::FAILURE
        }
    }
}
