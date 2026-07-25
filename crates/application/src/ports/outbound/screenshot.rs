//! `ScreenshotPort` — capture a URL to a PNG for visual QA. Optional
//! capability: hosts without a headless browser simply skip the pass.

use async_trait::async_trait;
use std::path::Path;

#[async_trait]
pub trait ScreenshotPort: Send + Sync {
    /// Capture `url` into `out` (PNG). `false` = could not capture (no
    /// browser, timeout) — callers treat that as "skip visual QA".
    async fn capture(&self, url: &str, out: &Path) -> bool;
}
