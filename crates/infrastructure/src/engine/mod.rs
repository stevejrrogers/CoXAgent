//! Agent-engine adapters and discovery.

pub mod any;
pub mod claude;
pub mod failover;
pub mod hermes;
pub mod metering;
pub mod mock;
pub mod opencode;
pub mod registry;
pub mod routing;
pub mod scripted;
pub mod transcript;

pub use any::AnyEngine;
pub use claude::ClaudeEngine;
pub use failover::{is_quota_wall, FailoverEngine, ALL_EXHAUSTED};
pub use hermes::HermesEngine;
pub use metering::{Meter, MeteringEngine};
pub use mock::MockEngine;
pub use opencode::OpencodeEngine;
pub use registry::{
    discover, discover_in, discover_opencode_models, discover_tooling, DetectedEngine,
    DetectedTool, Tooling,
};
pub use routing::RoutingEngine;
pub use scripted::ScriptedEngine;
pub use transcript::TranscriptEngine;

/// Loopback access to this project's own CoXAgent MCP endpoint (`/api/mcp`,
/// see `crates/presentation/src/server.rs`), handed to an engine adapter so a
/// spawned agent CLI can query the native code graph (`search_symbols`,
/// `symbol_refs`, ...) live instead of relying only on the static repo-map /
/// focus-block text baked into the prompt. `None` when no hub is reachable
/// for this run (e.g. a bare `coxagent check`).
#[derive(Debug, Clone)]
pub struct McpAccess {
    /// e.g. `http://127.0.0.1:4000/api/mcp`.
    pub url: String,
    /// Bearer token, when the hub has RBAC configured. `None` in open/local
    /// mode, where `/api/mcp` accepts unauthenticated loopback calls.
    pub token: Option<String>,
    /// This project's id, passed as the `project` argument on every tool call.
    pub project: String,
}

/// Prepend the command-output shim dir (from `COXAGENT_SHIM_DIR`) to the agent
/// subprocess's PATH, so heavy tool output the agent triggers is compressed
/// (rtk-style). No-op when unset. Applied to the agent process only — never the
/// hub itself, so CoXAgent's own git/tooling is unaffected.
/// The serde key for a role (e.g. `DEV-FEATURE`), used for per-role log files.
pub(crate) fn role_key(role: coxagent_domain::Role) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(crate) fn apply_shim_path(cmd: &mut tokio::process::Command) {
    if let Ok(shim) = std::env::var("COXAGENT_SHIM_DIR") {
        if !shim.is_empty() {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{shim}:{path}"));
        }
    }
}
