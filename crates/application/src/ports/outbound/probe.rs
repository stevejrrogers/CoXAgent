//! `ApiProbePort` — one HTTP GET against the deployed app, for API evidence.

use async_trait::async_trait;

/// A captured request/response pair (bodies capped by the adapter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiProof {
    pub status: u16,
    pub body_snippet: String,
}

#[async_trait]
pub trait ApiProbePort: Send + Sync {
    /// GET `url`; `None` when the app did not answer.
    async fn get(&self, url: &str) -> Option<ApiProof>;
}
