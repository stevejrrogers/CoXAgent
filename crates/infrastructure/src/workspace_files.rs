//! `FsWorkspaceFiles` — the [`WorkspaceFilesPort`] adapter over the real
//! filesystem.

use async_trait::async_trait;
use coxagent_application::ports::outbound::WorkspaceFilesPort;
use std::path::Path;

/// Plain `std::fs`-backed workspace file access.
#[derive(Default)]
pub struct FsWorkspaceFiles;

impl FsWorkspaceFiles {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl WorkspaceFilesPort for FsWorkspaceFiles {
    async fn read(&self, path: &Path) -> Option<String> {
        tokio::fs::read_to_string(path).await.ok()
    }

    async fn write(&self, path: &Path, content: &str) -> bool {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        tokio::fs::write(path, content).await.is_ok()
    }

    async fn delete(&self, path: &Path) -> bool {
        tokio::fs::remove_file(path).await.is_ok()
    }

    async fn list(&self, dir: &Path) -> Vec<coxagent_application::ports::outbound::FileMeta> {
        use coxagent_application::ports::outbound::FileMeta;
        let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(e)) = rd.next_entry().await {
            let path = e.path();
            if !path.is_file() {
                continue;
            }
            let meta = e.metadata().await.ok();
            out.push(FileMeta {
                modified_epoch: meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs()),
                size: meta.map_or(0, |m| m.len()),
                path,
            });
        }
        out
    }
}
