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

/// Resolve an agent-CLI binary (`opencode`, `claude`, `hermes`, ...) to an
/// absolute path. Prefer whatever `PATH` already resolves (a caller may set a
/// rich PATH), then fall back to the common install locations keyed off
/// `$HOME` — this keeps app-spawned operators working even when their inherited
/// PATH omits e.g. `~/.opencode/bin`. Returns the bare name only when nothing
/// is found anywhere, so resolution failures degrade exactly as before.
pub(crate) fn resolve_engine_binary(name: &str) -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if is_executable(&dir.join(name)) {
            return dir.join(name).display().to_string();
        }
    }
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    for dir in [
        home.join(".local/bin"),
        home.join(format!(".{name}/bin")),
        home.join(".opencode/bin"),
        std::path::PathBuf::from("/opt/homebrew/bin"),
        std::path::PathBuf::from("/usr/local/bin"),
    ] {
        if is_executable(&dir.join(name)) {
            return dir.join(name).display().to_string();
        }
    }
    name.to_owned()
}

fn is_executable(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = p.metadata() else { return false };
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_engine_binary_returns_absolute_path_when_installed() {
        // At least one agent CLI is installed on this machine; the resolver must
        // return an absolute path to an executable file, never a bare name.
        for name in ["opencode", "claude", "hermes"] {
            let bin = resolve_engine_binary(name);
            if bin == name {
                continue; // not installed here — nothing to assert
            }
            let p = std::path::Path::new(&bin);
            assert!(
                p.is_absolute(),
                "expected absolute path for {name}, got {bin}"
            );
            assert!(
                is_executable(p),
                "resolved {name} should be executable: {bin}"
            );
        }
    }

    #[test]
    fn is_executable_rejects_dirs_and_non_exec_files() {
        let td = std::env::temp_dir();
        assert!(!is_executable(&td)); // a directory is not an executable file

        let non_exec = td.join("cox_not_exec");
        std::fs::write(&non_exec, "x").unwrap();
        assert!(!is_executable(&non_exec));
        drop(std::fs::remove_file(&non_exec));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let exec = td.join("cox_is_exec");
            std::fs::write(&exec, "#!/bin/sh\n").unwrap();
            let mut perms = std::fs::metadata(&exec).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&exec, perms).unwrap();
            assert!(is_executable(&exec));
            drop(std::fs::remove_file(&exec));
        }
    }
}
