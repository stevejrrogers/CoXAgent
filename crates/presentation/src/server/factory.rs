//! The onboarding factory contract: the composition root injects this port so
//! the presentation layer can build a project on demand without touching the
//! filesystem or the hub registry itself. The failure type is classified at
//! the source (CXA-B129): an expected client conflict — the target workspace
//! already holds tickets — must reach the API client as HTTP 409, never a 500.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use super::ProjectHandle;

/// Builds a fresh project on demand (scaffold + register), injected by the
/// composition root so the presentation layer stays free of infrastructure.
/// Takes a [`NewProjectReq`], returns a ready [`ProjectHandle`] or a
/// classified [`FactoryError`].
pub type ProjectFactory = Arc<
    dyn Fn(NewProjectReq) -> Pin<Box<dyn Future<Output = Result<ProjectHandle, FactoryError>> + Send>>
        + Send
        + Sync,
>;

/// A request to create a project. `existing` adopts a codebase (brownfield);
/// `goal` seeds the project context (from AI-assisted goal drafting).
#[derive(Clone, Default)]
pub struct NewProjectReq {
    pub name: String,
    pub alias: Option<String>,
    pub existing: Option<PathBuf>,
    /// Import straight from a git URL: the factory clones it into the
    /// project workspace, then adopts it like any existing codebase (remote
    /// auto-detected, config pre-filled).
    pub git_url: Option<String>,
    pub goal: Option<String>,
}

/// Deregisters a project (removes it from the hub registry), injected by the
/// composition root. Returns an error message on failure.
pub type ProjectRemover =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

/// A project-factory failure, classified at the source so the HTTP layer can
/// map an expected client conflict to 409 instead of a 500 (CXA-B129).
#[derive(Debug, Clone)]
pub struct FactoryError {
    pub message: String,
    /// True when the failure is an expected client-side conflict — the target
    /// workspace already holds tickets — mapped to HTTP 409, not 500.
    pub conflict: bool,
}

impl FactoryError {
    /// A genuine server-side fault (HTTP 500).
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            conflict: false,
        }
    }

    /// An expected client conflict (HTTP 409).
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            conflict: true,
        }
    }
}

impl std::fmt::Display for FactoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FactoryError {}
