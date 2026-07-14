//! `GitPort` — the boundary over local version control (branch, stage, commit,
//! push) for the codebase. Drives the git flow: a branch + commit per ticket,
//! pushed to the configured remote. Adapters (git CLI) live in infrastructure.
//! Forge operations (opening PRs/MRs, CI status) live behind a separate port.

use crate::error::PortError;
use async_trait::async_trait;
use std::path::Path;

/// The identity a commit is authored under.
#[derive(Debug, Clone)]
pub struct GitAuthor {
    pub name: String,
    /// Use a `…@users.noreply.github.com` address to avoid email-privacy push
    /// rejections on GitHub.
    pub email: String,
}

/// Local git operations on a codebase directory.
#[async_trait]
pub trait GitPort: Send + Sync {
    /// Whether `work_dir` is a git repository.
    async fn is_repo(&self, work_dir: &Path) -> bool;

    /// The current branch name (e.g. `main`).
    ///
    /// # Errors
    /// [`PortError::Backend`] if git cannot be run or reports no branch.
    async fn current_branch(&self, work_dir: &Path) -> Result<String, PortError>;

    /// Create `branch` from the current HEAD (or switch to it if it exists) and
    /// check it out.
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure.
    async fn checkout_branch(&self, work_dir: &Path, branch: &str) -> Result<(), PortError>;

    /// Stage every change and commit under `author` with `message`. Returns the
    /// commit sha, or `None` when the tree was clean (nothing to commit).
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure other than an empty commit.
    async fn commit_all(
        &self,
        work_dir: &Path,
        message: &str,
        author: &GitAuthor,
    ) -> Result<Option<String>, PortError>;

    /// Push `branch` to `origin`, setting upstream.
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git/network failure.
    async fn push(&self, work_dir: &Path, branch: &str) -> Result<(), PortError>;
}
