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
/// Snapshot of the uncommitted working tree (see [`GitPort::working_tree`]).
#[derive(Debug, Clone, Default)]
pub struct WorkingTreeDiff {
    /// Paths changed vs HEAD plus untracked files, repo-relative.
    pub changed_paths: Vec<String>,
    /// `git diff HEAD` — full unified diff.
    pub full_diff: String,
    /// `git diff HEAD -U0` — no context lines.
    pub unified0_diff: String,
    /// For each changed path that contains one: the 0-based line of its first
    /// `#[cfg(test)]`. Read by the adapter so the gate that asks "did this
    /// change land inside a test module" stays a pure function.
    pub cfg_test_line: std::collections::BTreeMap<String, usize>,
}

#[async_trait]
pub trait GitPort: Send + Sync {
    /// The working tree's uncommitted change, in the three shapes the DoD
    /// gates read: which paths moved (tracked vs HEAD, plus untracked), the
    /// full diff, and the zero-context diff whose line numbers are the changed
    /// lines themselves.
    ///
    /// A port method because the gates were shelling out to `git` from the
    /// application layer — IO the architecture says goes through an adapter,
    /// and the reason those gates could only be tested against real temp repos.
    /// Raw git plumbing the forge-hygiene flow needs, behind one honest door:
    /// run `git` with `args` in `work_dir`, succeed-or-not plus stdout. The
    /// operations (fetch, merge-base, rebase-by-merge, rev-list, ls-remote)
    /// are too shell-shaped to earn one port method each, but they are still
    /// IO — and IO lives in the adapter, where the hexagonal ratchet can see
    /// that the application never spawns a process itself.
    ///
    /// Default: failure with empty output, so a double that doesn't care
    /// reads as "git did nothing".
    async fn raw(&self, _work_dir: &Path, _args: &[&str]) -> (bool, String) {
        (false, String::new())
    }

    ///
    /// Default: an empty tree, which reads as "nothing changed". Every REAL
    /// git adapter must override this — the default exists for test doubles,
    /// and an adapter that forgets it quietly blinds the DoD gates.
    ///
    /// # Errors
    /// [`PortError::Backend`] when `git` cannot be run.
    async fn working_tree(&self, _work_dir: &Path) -> Result<WorkingTreeDiff, PortError> {
        Ok(WorkingTreeDiff::default())
    }

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

    /// Bring the latest `origin/<base>` INTO the current branch (fetch + merge).
    /// This is the "PRs are born mergeable" law: called before every push/PR so
    /// conflicts surface — and get resolved — on the branch, never in the queue.
    ///
    /// # Errors
    /// [`PortError::Backend`] if git cannot fetch or the merge cannot start.
    async fn sync_base(&self, work_dir: &Path, base: &str) -> Result<SyncBase, PortError>;

    /// Abort an in-progress merge, restoring the branch to its pre-merge state.
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure.
    async fn abort_merge(&self, work_dir: &Path) -> Result<(), PortError>;

    /// HEAD's full commit sha. Used to record exactly what a deploy attempt
    /// built/ran, so a later rollback names an exact commit rather than
    /// "whatever HEAD drifted to". Default: unsupported.
    ///
    /// # Errors
    /// [`PortError::Backend`] if git cannot resolve HEAD.
    async fn head_sha(&self, work_dir: &Path) -> Result<String, PortError> {
        let _ = work_dir;
        Err(PortError::Backend("head_sha: not supported".to_owned()))
    }

    /// Point a local, non-pushed ref (e.g. `refs/coxagent/last-good`) at `sha`.
    /// Used to mark the last deploy that passed both `deploy()` and
    /// `run_tests()` — a ref survives ticket-branch deletion after a
    /// squash-merge, unlike tracking a branch tip. Default: unsupported.
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure.
    async fn update_ref(&self, work_dir: &Path, refname: &str, sha: &str) -> Result<(), PortError> {
        let _ = (work_dir, refname, sha);
        Err(PortError::Backend("update_ref: not supported".to_owned()))
    }

    /// Park the working tree's uncommitted state (staged, unstaged and
    /// untracked) as one commit reachable ONLY via `ref_name` — the slot-WIP
    /// checkpoint (CXA-F318). HEAD, the current branch and the working tree
    /// are left exactly as they were: the tree stays dirty, the ref holds the
    /// parked work. Returns the parked commit's sha, or `None` when the tree
    /// was clean (nothing to park — no ref is created).
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure (e.g. an index lock).
    async fn checkpoint_tree(
        &self,
        work_dir: &Path,
        ref_name: &str,
        message: &str,
        author: &GitAuthor,
    ) -> Result<Option<String>, PortError> {
        let _ = (work_dir, ref_name, message, author);
        Err(PortError::Backend(
            "checkpoint_tree: not supported".to_owned(),
        ))
    }

    /// Overlay the content parked at `ref_name` (see [`GitPort::checkpoint_tree`])
    /// onto the working tree — restore-only, HEAD and the current branch are
    /// never touched, and files added since the park stay put.
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure or an unresolvable ref.
    async fn restore_tree(&self, work_dir: &Path, ref_name: &str) -> Result<(), PortError> {
        let _ = (work_dir, ref_name);
        Err(PortError::Backend("restore_tree: not supported".to_owned()))
    }

    /// Create a detached secondary worktree at `path`, checked out at `sha`.
    /// Rollback deploys run here so the live `work_dir` is never touched — no
    /// race with the concurrent `checkout_branch` calls elsewhere in the
    /// leader tail. Callers `worktree_remove` first so `path` is always
    /// freshly created. Default: unsupported.
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure.
    async fn worktree_add(&self, work_dir: &Path, path: &Path, sha: &str) -> Result<(), PortError> {
        let _ = (work_dir, path, sha);
        Err(PortError::Backend("worktree_add: not supported".to_owned()))
    }

    /// Remove a worktree created by [`GitPort::worktree_add`]. Best-effort:
    /// callers ignore the error since `path` may not exist yet. Default: no-op.
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure.
    async fn worktree_remove(&self, work_dir: &Path, path: &Path) -> Result<(), PortError> {
        let _ = (work_dir, path);
        Ok(())
    }

    /// Paths that differ between two commits — used to detect a migration
    /// shipped since the last known-good deploy (rolling back the app code
    /// without the DB schema could be unsafe). Default: no diff available, so
    /// callers see no migration paths touched.
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure.
    async fn changed_paths(
        &self,
        work_dir: &Path,
        from_sha: &str,
        to_sha: &str,
    ) -> Result<Vec<String>, PortError> {
        let _ = (work_dir, from_sha, to_sha);
        Ok(Vec::new())
    }

    /// Create a lightweight or annotated tag on `ref_target` (sha or branch).
    /// `author` supplies the committer identity for the annotated tag, so the
    /// call never depends on a machine's ambient `git config user.name/email`
    /// (mirrors `commit_all`). Default: unsupported.
    ///
    /// # Errors
    /// [`PortError::Backend`] on a git failure.
    async fn create_tag(
        &self,
        work_dir: &Path,
        name: &str,
        ref_target: &str,
        author: &GitAuthor,
    ) -> Result<(), PortError> {
        let _ = (work_dir, name, ref_target, author);
        Err(PortError::Backend("create_tag: not supported".to_owned()))
    }

    /// Whether a tag with `name` already exists in the repository. Default:
    /// false.
    async fn tag_exists(&self, work_dir: &Path, name: &str) -> bool {
        let _ = (work_dir, name);
        false
    }
}

/// Outcome of [`GitPort::sync_base`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncBase {
    /// Branch already contained the base tip — nothing to do.
    UpToDate,
    /// Base merged in cleanly (a merge commit was created).
    Merged,
    /// The merge stopped on conflicts; the listed files have conflict markers
    /// and the merge is left in progress for a resolver to finish.
    Conflicts(Vec<String>),
}
