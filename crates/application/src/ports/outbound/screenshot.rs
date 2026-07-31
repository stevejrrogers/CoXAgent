//! `ScreenshotPort` — capture a URL to PNG bytes for visual QA. Optional
//! capability: hosts without a headless browser simply skip the pass.
//!
//! Returns bytes rather than writing a file: the temp file was the adapter's
//! implementation detail, and handing it back forced the CALLER to do
//! filesystem IO the application layer is not allowed.

use async_trait::async_trait;

#[async_trait]
pub trait ScreenshotPort: Send + Sync {
    /// Capture `url` as a PNG. `None` = could not capture (no browser,
    /// timeout) — callers treat that as "skip visual QA".
    async fn capture(&self, url: &str) -> Option<Vec<u8>>;
}
