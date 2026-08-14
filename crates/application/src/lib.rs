//! CoXAgent application layer — use cases and the ports they depend on.
//! Depends only on the domain crate; infrastructure implements the outbound
//! ports, presentation drives the inbound ones.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod auth;
pub mod codegraph;
pub mod config;
pub mod config_parse;
pub mod conformance;
pub mod error;
pub mod faults;
pub mod metrics;
pub mod parsing;
pub mod policy;
pub mod ports;
pub mod prompts;
pub mod selection;
pub mod sprint;
pub mod state;
pub mod system_chat;
pub mod tokens;
pub mod ts;
pub mod use_cases;
pub mod verify_cache;

pub use auth::{AuthPort, AuthRole, AuthUser, LoginResult, TokenInfo};
pub use config::{
    BudgetCaps, Config, DeployConfig, EngineChoice, EngineKind, EngineMapping, LiveBudget, Mode,
    PolicyConfig, WorkflowConfig,
};
pub use config_parse::{parse_config, ConfigParseError};
pub use error::{AppError, PortError};
pub use state::{
    Attachment, Channel, ChatMsg, Comment, DesignSystem, DocPage, HealthCheckResult, Milestone,
    PrReview, ProjectState, Reaction, Spend, Sprint, GENERAL_CHANNEL, SCHEMA_VERSION,
};
pub use system_chat::{ChatContext, ProjectRef, SystemChat, UserRef, Webhook};

/// Test-only filesystem adapter: the real disk behind the files port, for
/// tests that build fixtures in a temp dir. `#[cfg(test)]` code may use
/// `std::fs` — the hexagonal ratchet checks the production half only.
#[cfg(test)]
pub(crate) mod test_fs {
    use crate::ports::outbound::{FileMeta, WorkspaceFilesPort};
    use std::path::{Path, PathBuf};

    pub(crate) struct StdFsFiles;

    #[async_trait::async_trait]
    impl WorkspaceFilesPort for StdFsFiles {
        async fn read(&self, path: &Path) -> Option<String> {
            std::fs::read_to_string(path).ok()
        }
        async fn write(&self, path: &Path, content: &str) -> bool {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(path, content).is_ok()
        }
        async fn write_bytes(&self, path: &Path, bytes: &[u8]) -> bool {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(path, bytes).is_ok()
        }
        async fn delete(&self, path: &Path) -> bool {
            std::fs::remove_file(path).is_ok()
        }
        async fn stat(&self, path: &Path) -> Option<FileMeta> {
            let md = std::fs::metadata(path).ok()?;
            let modified_epoch = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            Some(FileMeta {
                path: path.to_path_buf(),
                modified_epoch,
                size: md.len(),
            })
        }
        async fn list_recursive(&self, dir: &Path) -> Vec<PathBuf> {
            let mut stack = vec![dir.to_path_buf()];
            let mut out = Vec::new();
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else {
                    continue;
                };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        out.push(p);
                    }
                }
            }
            out
        }
        async fn list_dirs(&self, dir: &Path) -> Vec<PathBuf> {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return Vec::new();
            };
            let mut out: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            out.sort();
            out
        }
        async fn list(&self, dir: &Path) -> Vec<FileMeta> {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for e in rd.flatten() {
                let path = e.path();
                if !path.is_file() {
                    continue;
                }
                let Ok(md) = e.metadata() else { continue };
                let modified_epoch = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                out.push(FileMeta {
                    path,
                    modified_epoch,
                    size: md.len(),
                });
            }
            out
        }
    }
}
