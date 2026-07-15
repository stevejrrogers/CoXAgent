//! `DocStorePort` — a server-side document store for the living documentation,
//! independent of a project's `state.json`. When a backend (e.g. MongoDB) is
//! configured it becomes the system of record for docs; otherwise the app falls
//! back to per-project state. Pages are scoped by project id.

use crate::error::PortError;
use crate::state::DocPage;
use async_trait::async_trait;

/// Server-side persistence for documentation pages, keyed by `(project, id)`.
#[async_trait]
pub trait DocStorePort: Send + Sync {
    /// All pages for a project, newest activity is not implied by order.
    async fn list(&self, project: &str) -> Result<Vec<DocPage>, PortError>;

    /// One page by id, or `None` if it does not exist.
    async fn get(&self, project: &str, id: &str) -> Result<Option<DocPage>, PortError>;

    /// Create or replace a page (upsert by `page.id`).
    async fn upsert(&self, project: &str, page: &DocPage) -> Result<(), PortError>;

    /// Remove a page; returns whether one existed.
    async fn delete(&self, project: &str, id: &str) -> Result<bool, PortError>;
}
