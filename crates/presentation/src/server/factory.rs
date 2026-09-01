//! The onboarding factory contract: the composition root injects this port so
//! the presentation layer can build a project on demand without touching the
//! filesystem or the hub registry itself. The failure type is classified at
//! the source (CXA-B129, CXA-B138, CXA-B139): an expected client conflict —
//! the target workspace already holds tickets — must reach the API client as
//! HTTP 409, and invalid client input (a path-traversing alias, an unsupported
//! git URL scheme) as HTTP 400, never a 500 for either.

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
    dyn Fn(
            NewProjectReq,
        ) -> Pin<Box<dyn Future<Output = Result<ProjectHandle, FactoryError>> + Send>>
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

/// Which HTTP class a project-factory failure belongs to (CXA-B129, CXA-B138,
/// CXA-B139). Classified at the source so the HTTP layer never has to guess
/// from the message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactoryErrorKind {
    /// A genuine server-side fault (HTTP 500).
    Internal,
    /// An expected client conflict — the target workspace already holds
    /// tickets (HTTP 409).
    Conflict,
    /// Invalid client input — a pure request-validation failure such as a
    /// path-traversing alias (CXA-B138) or an unsupported git URL scheme
    /// (CXA-B139) (HTTP 400).
    BadRequest,
}

/// A project-factory failure, classified at the source so the HTTP layer can
/// map an expected client conflict to 409 instead of a 500 (CXA-B129), a
/// refused request to 400 instead of a 500 (CXA-B138), and unsupported client
/// input such as a bad git URL scheme to 400 (CXA-B139).
#[derive(Debug, Clone)]
pub struct FactoryError {
    pub message: String,
    /// The failure class — drives the HTTP status mapping.
    pub kind: FactoryErrorKind,
}

impl FactoryError {
    /// A genuine server-side fault (HTTP 500).
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: FactoryErrorKind::Internal,
        }
    }

    /// An expected client conflict (HTTP 409).
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: FactoryErrorKind::Conflict,
        }
    }

    /// Invalid client input — the request itself can never succeed (HTTP 400,
    /// CXA-B138 path-traversing alias, CXA-B139 bad git URL scheme).
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: FactoryErrorKind::BadRequest,
        }
    }
}

impl std::fmt::Display for FactoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FactoryError {}
