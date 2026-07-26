//! `ForgePort` — the boundary over a code-hosting provider's API (GitHub /
//! GitLab): open a pull/merge request, list open ones, read CI status, and
//! merge. Local git (branch/commit/push) is a separate port ([`super::git`]).
//! Adapters live in infrastructure.

use crate::error::PortError;
use async_trait::async_trait;

/// A pull request / merge request as surfaced to the dashboard.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PullRequest {
    /// Provider number (`#123`).
    pub number: u64,
    pub title: String,
    /// Source branch (e.g. `feat/CXC-123`).
    pub head: String,
    /// Target branch (e.g. `main`).
    pub base: String,
    /// Web URL.
    pub url: String,
    /// Author login.
    pub author: String,
    /// CI rollup: `"passing"` / `"failing"` / `"pending"` / `"none"`.
    pub ci: String,
    /// Whether the branch merges cleanly.
    pub mergeable: bool,
    /// RFC3339 creation time.
    pub created: String,
}

/// The forge (code host) API for a project's repository.
#[async_trait]
pub trait ForgePort: Send + Sync {
    /// Open a pull/merge request from `head` into `base`. Returns the PR.
    ///
    /// # Errors
    /// [`PortError::Backend`] on an API/CLI failure.
    async fn open_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, PortError>;

    /// List open pull/merge requests, newest first.
    ///
    /// # Errors
    /// [`PortError::Backend`] on an API/CLI failure.
    async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError>;

    /// The unified diff for a PR (for the in-app review view).
    ///
    /// # Errors
    /// [`PortError::Backend`] on an API/CLI failure.
    async fn pr_diff(&self, number: u64) -> Result<String, PortError>;

    /// Merge a PR (squash). Returns once the merge is accepted.
    ///
    /// # Errors
    /// [`PortError::Backend`] if the merge is refused or fails.
    async fn merge_pr(&self, number: u64) -> Result<(), PortError>;

    /// Leave a review requesting changes, with `comment` for the author.
    ///
    /// # Errors
    /// [`PortError::Backend`] on an API/CLI failure.
    async fn request_changes(&self, number: u64, comment: &str) -> Result<(), PortError>;

    /// Close a PR without merging.
    ///
    /// # Errors
    /// [`PortError::Backend`] on an API/CLI failure.
    async fn close_pr(&self, number: u64) -> Result<(), PortError>;

    /// Unaddressed review feedback on a PR: change-request reviews submitted
    /// AFTER the branch's latest commit (older ones were already addressed by a
    /// push). Empty = nothing to fix. Default: unsupported → empty.
    ///
    /// # Errors
    /// [`PortError::Backend`] on an API/CLI failure.
    /// PR numbers recently closed WITHOUT merging, with their head branch —
    /// a human rejecting work is the most expensive teaching signal there is.
    /// Default: none (forges without the query).
    async fn closed_unmerged(&self) -> Result<Vec<(u64, String)>, PortError> {
        Ok(Vec::new())
    }

    async fn pr_feedback(&self, _number: u64) -> Result<Vec<PrFeedback>, PortError> {
        Ok(Vec::new())
    }

    /// Leave a plain comment on a PR (e.g. "addressed the feedback").
    ///
    /// # Errors
    /// [`PortError::Backend`] on an API/CLI failure.
    async fn comment_pr(&self, _number: u64, _body: &str) -> Result<(), PortError> {
        Ok(())
    }
}

/// One piece of review feedback awaiting a fix.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PrFeedback {
    /// Reviewer login.
    pub author: String,
    /// The review body (what to change).
    pub body: String,
    /// RFC3339 submission time.
    pub at: String,
}
