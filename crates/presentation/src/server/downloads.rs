// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Upload size limits, MIME detection, filename sanitization, and the fallback
//! local-disk blob storage adapter.

use super::*;

/// Max upload size (bytes) — generous for images/docs, bounded to protect disk.
pub(super) const UPLOAD_MAX: usize = 25 * 1024 * 1024;

/// Best-effort MIME from a file extension (for serving uploads).
pub(super) fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt" | "log" | "md") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Sanitize an original filename to a safe stored suffix (keeps the extension).
pub(super) fn sanitize_name(orig: &str) -> String {
    orig.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Default blob storage: local disk under a root, used when no S3/MinIO backend
/// is injected. Keys are relative paths (e.g. `chat/<file>`).
pub(super) struct DiskStorage {
    pub(super) root: PathBuf,
}

#[async_trait::async_trait]
impl coxagent_application::ports::outbound::StoragePort for DiskStorage {
    async fn put(
        &self,
        key: &str,
        data: &[u8],
        _mime: &str,
    ) -> Result<(), coxagent_application::PortError> {
        if key.contains("..") {
            return Err(coxagent_application::PortError::Backend(
                "bad key".to_owned(),
            ));
        }
        let path = self.root.join(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| coxagent_application::PortError::Backend(e.to_string()))?;
        }
        std::fs::write(&path, data)
            .map_err(|e| coxagent_application::PortError::Backend(e.to_string()))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, coxagent_application::PortError> {
        if key.contains("..") {
            return Err(coxagent_application::PortError::Backend(
                "bad key".to_owned(),
            ));
        }
        std::fs::read(self.root.join(key))
            .map_err(|e| coxagent_application::PortError::Backend(e.to_string()))
    }
}
