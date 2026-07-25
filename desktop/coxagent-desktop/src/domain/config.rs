//! Desktop configuration, resolved from the environment but testable in
//! isolation via [`DesktopConfig::resolve`].

use std::path::PathBuf;
use std::time::Duration;

/// Default dashboard port (matches the Swift shell and `coxagent hub`).
pub const DEFAULT_PORT: u16 = 4000;

/// How the launch use case waits for the hub: attempts × interval ≈ 24 s,
/// same ceiling as the Swift shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchPolicy {
    pub port: u16,
    pub ready_attempts: u32,
    pub ready_interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopConfig {
    pub port: u16,
    /// Per-user workspace: registry, project state, logs.
    pub workspace: PathBuf,
}

impl DesktopConfig {
    /// Resolve from real env + home dir.
    pub fn from_env() -> Self {
        Self::resolve(
            |k| std::env::var(k).ok(),
            dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
        )
    }

    /// Pure resolution: `COXAGENT_DESKTOP_PORT` overrides the port (invalid
    /// values fall back to the default); the workspace is `<home>/CoXAgent`.
    pub fn resolve(var: impl Fn(&str) -> Option<String>, home: PathBuf) -> Self {
        let port = var("COXAGENT_DESKTOP_PORT")
            .and_then(|v| v.trim().parse::<u16>().ok())
            .filter(|p| *p != 0)
            .unwrap_or(DEFAULT_PORT);
        Self {
            port,
            workspace: home.join("CoXAgent"),
        }
    }

    pub fn dashboard_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    pub fn policy(&self) -> LaunchPolicy {
        LaunchPolicy {
            port: self.port,
            ready_attempts: 80,
            ready_interval: Duration::from_millis(300),
        }
    }
}

/// The bundled hub binary is named `cox-server` (not `coxagent`) so it never
/// collides case-insensitively with the shell executable `CoXAgent` on
/// Windows/macOS filesystems.
pub fn hub_binary_name() -> &'static str {
    if cfg!(windows) {
        "cox-server.exe"
    } else {
        "cox-server"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_when_env_empty() {
        let cfg = DesktopConfig::resolve(no_env, PathBuf::from("/home/u"));
        assert_eq!(cfg.port, DEFAULT_PORT);
        assert_eq!(cfg.workspace, PathBuf::from("/home/u/CoXAgent"));
        assert_eq!(cfg.dashboard_url(), "http://127.0.0.1:4000/");
    }

    #[test]
    fn port_override_from_env() {
        let cfg = DesktopConfig::resolve(
            |k| (k == "COXAGENT_DESKTOP_PORT").then(|| " 8123 ".into()),
            PathBuf::from("/h"),
        );
        assert_eq!(cfg.port, 8123);
        assert_eq!(cfg.dashboard_url(), "http://127.0.0.1:8123/");
    }

    #[test]
    fn invalid_or_zero_port_falls_back() {
        for bad in ["abc", "0", "70000", ""] {
            let cfg = DesktopConfig::resolve(
                |k| (k == "COXAGENT_DESKTOP_PORT").then(|| bad.into()),
                PathBuf::from("/h"),
            );
            assert_eq!(cfg.port, DEFAULT_PORT, "input {bad:?}");
        }
    }

    #[test]
    fn policy_matches_config_port() {
        let cfg = DesktopConfig::resolve(no_env, PathBuf::from("/h"));
        let p = cfg.policy();
        assert_eq!(p.port, cfg.port);
        assert!(p.ready_attempts as u128 * p.ready_interval.as_millis() >= 20_000);
    }

    #[test]
    fn hub_binary_name_matches_os() {
        if cfg!(windows) {
            assert_eq!(hub_binary_name(), "cox-server.exe");
        } else {
            assert_eq!(hub_binary_name(), "cox-server");
        }
    }
}
