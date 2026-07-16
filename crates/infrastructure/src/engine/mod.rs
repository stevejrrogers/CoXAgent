//! Agent-engine adapters and discovery.

pub mod any;
pub mod claude;
pub mod failover;
pub mod metering;
pub mod mock;
pub mod opencode;
pub mod registry;
pub mod scripted;
pub mod transcript;

pub use any::AnyEngine;
pub use claude::ClaudeEngine;
pub use failover::{is_quota_wall, FailoverEngine, ALL_EXHAUSTED};
pub use metering::{Meter, MeteringEngine};
pub use mock::MockEngine;
pub use opencode::OpencodeEngine;
pub use registry::{
    discover, discover_in, discover_tooling, DetectedEngine, DetectedTool, Tooling,
};
pub use scripted::ScriptedEngine;
pub use transcript::TranscriptEngine;

/// Prepend the command-output shim dir (from `COXAGENT_SHIM_DIR`) to the agent
/// subprocess's PATH, so heavy tool output the agent triggers is compressed
/// (rtk-style). No-op when unset. Applied to the agent process only — never the
/// hub itself, so CoXAgent's own git/tooling is unaffected.
pub(crate) fn apply_shim_path(cmd: &mut tokio::process::Command) {
    if let Ok(shim) = std::env::var("COXAGENT_SHIM_DIR") {
        if !shim.is_empty() {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{shim}:{path}"));
        }
    }
}
